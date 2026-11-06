#!/usr/bin/env python3
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
"""Compares criterion benchmarks between trees without the bias that code layout puts into
single measurements. See benchmark/README.md.

Each tree's benchmarks are compiled once, with a linker wrapper that records how rustc
links every bench executable. Each executable is then linked again once per seed with lld's
--randomize-section-padding, which pads between functions and data objects (rustc gives
each its own section), so every seed is another code and data layout. The builds run
interleaved on primary cores sharing one L3, and every benchmark of each tree is compared
with the first tree's over seeds, by Yuen's 95% interval (Welch's, on 20% trimmed means)."""
import argparse
import glob
import json
import math
import os
import re
import shutil
import statistics as st
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = subprocess.run(['git', '-C', HERE, 'rev-parse', '--show-toplevel'],
                      capture_output=True, text=True, check=True).stdout.strip()

# Two-sided 95% Student t quantiles by degrees of freedom.
T975 = [(1, 12.71), (2, 4.30), (3, 3.18), (4, 2.78), (5, 2.57), (6, 2.45), (7, 2.36),
        (8, 2.31), (9, 2.26), (10, 2.23), (12, 2.18), (14, 2.14), (16, 2.12), (20, 2.09),
        (30, 2.04), (60, 2.00)]

# Settings the code under test could read; none of the caller's may leak into a measurement.
FOREIGN_ENV = ('DD_', '_DD_', 'OTEL_')


def log(msg):
    print(msg, file=sys.stderr, flush=True)


def die(msg):
    log(f'error: {msg}')
    sys.exit(1)


def run(cmd, **kw):
    return subprocess.run(cmd, check=True, **kw)


def git(*args, cwd=REPO, **kw):
    return subprocess.run(['git', '-C', cwd, *args], capture_output=True, text=True, **kw)


# Trees ---------------------------------------------------------------------------------------

def checkout(tree, dest):
    """Copies a checkout's tracked and untracked (not ignored) files, any other directory
    whole, or exports a revision with its initialized submodules."""
    os.makedirs(dest)
    if os.path.isdir(tree):
        if git('rev-parse', '--is-inside-work-tree', cwd=tree).returncode:
            shutil.copytree(tree, dest, symlinks=True, dirs_exist_ok=True)
            return
        files = git('ls-files', '-z', '--cached', '--others', '--exclude-standard',
                    cwd=tree, check=True).stdout.split('\0')
        for f in filter(None, files):
            src = os.path.join(tree, f)
            if os.path.isdir(src) and not os.path.islink(src):  # a submodule
                shutil.copytree(src, os.path.join(dest, f), symlinks=True, dirs_exist_ok=True)
            elif os.path.lexists(src):
                os.makedirs(os.path.dirname(os.path.join(dest, f)), exist_ok=True)
                shutil.copy2(src, os.path.join(dest, f), follow_symlinks=False)
        return
    if git('rev-parse', '--verify', '--quiet', f'{tree}^{{commit}}').returncode:
        die(f'{tree} is neither a directory nor a git revision')
    archive = subprocess.Popen(['git', '-C', REPO, 'archive', tree], stdout=subprocess.PIPE)
    run(['tar', '-x', '-C', dest], stdin=archive.stdout)
    if archive.wait():
        die(f'git archive {tree} failed')
    # git archive leaves submodules empty; fill those the local checkout can provide.
    for line in git('ls-tree', '-r', tree, check=True).stdout.splitlines():
        meta, path = line.split('\t', 1)
        mode, kind, sha = meta.split()
        if kind != 'commit':
            continue
        sub = os.path.join(REPO, path)
        if git('cat-file', '-e', f'{sha}^{{commit}}', cwd=sub).returncode:
            log(f'warning: submodule {path} at {sha[:9]} is not available locally; '
                'benchmarks that read it will fail')
            continue
        archive = subprocess.Popen(['git', '-C', sub, 'archive', sha], stdout=subprocess.PIPE)
        run(['tar', '-x', '-C', os.path.join(dest, path)], stdin=archive.stdout)
        archive.wait()


def tree_name(tree):
    if os.path.isdir(tree):
        return tree
    short = git('rev-parse', '--short', f'{tree}^{{commit}}').stdout.strip()
    return tree if short.startswith(tree) or tree.startswith(short) else f'{tree} ({short})'


def bench_packages(src):
    """{package name: (manifest dir, feature names)} for the tree's crates with benchmarks."""
    meta = json.loads(run(['cargo', 'metadata', '--no-deps', '--format-version', '1'],
                          cwd=src, capture_output=True, text=True).stdout)
    return {p['name']: (os.path.dirname(p['manifest_path']), set(p['features']))
            for p in meta['packages'] if any('bench' in t['kind'] for t in p['targets'])}


def host_triple():
    out = run(['rustc', '-vV'], capture_output=True, text=True, cwd=REPO).stdout
    return re.search(r'^host: (\S+)$', out, re.M).group(1)


def build(i, tree, a, out):
    """Compiles a tree's benchmarks once, recording each bench executable's link. Returns its
    units: {(package, bench target): {'exe', 'record', 'cwd'}}."""
    t = os.path.join(out, f't{i}')
    src, links, tmp = (os.path.join(t, d) for d in ('src', 'links', 'tmp'))
    checkout(tree, src)
    os.makedirs(links)
    os.makedirs(tmp)
    available = bench_packages(src)
    wanted = a.package or sorted(available)
    args = []
    for pkg in wanted:
        if pkg not in available:
            log(f'{tree_name(tree)}: no benchmarks in {pkg}, skipped')
            continue
        args += ['-p', pkg]
        features = run([os.path.join(HERE, 'bench_features.sh'), pkg],
                       capture_output=True, text=True).stdout.split()
        args += [f'--features={f}' for f in features if f.split('/', 1)[1] in available[pkg][1]]
    for bench in a.bench:
        args += ['--bench', bench]
    if not any(x == '-p' for x in args):
        return {}
    env = dict(os.environ)
    triple = host_triple().upper().replace('-', '_')
    env.update({
        # A target directory per tree: cargo hashes workspace members by their path within the
        # workspace, so two trees would overwrite each other's rlibs, which relinking reads.
        'CARGO_TARGET_DIR': os.path.join(t, 'target'),
        f'CARGO_TARGET_{triple}_LINKER': os.path.join(HERE, 'stable-link-recorder.py'),
        'STABLE_LINK_RECORD_DIR': links,
        'TMPDIR': tmp,
        'CARGO_INCREMENTAL': '0',
    })
    blog = os.path.join(t, 'build.log')
    with open(blog, 'w') as errors:
        p = subprocess.run(['cargo', 'bench', '--no-run', '--message-format=json-render-diagnostics',
                            *args], cwd=src, env=env, stdout=subprocess.PIPE, stderr=errors, text=True)
    if p.returncode:
        with open(blog) as f:
            die(f'building {tree_name(tree)} failed ({blog}):\n' + ''.join(f.readlines()[-30:]))
    dirs = {v[0]: k for k, v in available.items()}
    units = {}
    for line in p.stdout.splitlines():
        msg = json.loads(line)
        if msg.get('reason') != 'compiler-artifact' or 'bench' not in msg['target']['kind']:
            continue
        exe = msg['executable']
        record = os.path.join(links, os.path.basename(exe) + '.json')
        if not os.path.exists(record):
            die(f'no link was recorded for {exe}; remove {out} and try again')
        pkg_dir = os.path.dirname(msg['manifest_path'])
        units[(dirs[pkg_dir], msg['target']['name'])] = {'exe': exe, 'record': record, 'cwd': pkg_dir}
    return units


def relink(unit, seed, dest):
    """Links a recorded executable again as `dest`, with lld padding sections by `seed`, or
    as rustc linked it when `seed` is None."""
    with open(unit['record']) as f:
        rec = json.load(f)
    args = [dest if prev == '-o' else arg for prev, arg in zip([None] + rec['args'], rec['args'])]
    if not any(arg.startswith('-fuse-ld=') for arg in args):
        args.append('-fuse-ld=lld')  # toolchains before 1.90 link with the system's ld
    if seed is not None:
        args.append(f'-Wl,--randomize-section-padding={seed}')
    p = subprocess.run([rec['linker'], *args], cwd=rec['cwd'], capture_output=True, text=True)
    if p.returncode:
        die(f'relinking {unit["exe"]} failed; lld 20 or later is required for '
            f'--randomize-section-padding:\n{p.stderr[-3000:]}')


# Running -------------------------------------------------------------------------------------

def read(path):
    with open(path) as f:
        return f.read().strip()


def cpu_list(text):
    cpus = set()
    for part in filter(None, text.split(',')):
        first, _, last = part.partition('-')
        cpus.update(range(int(first), int(last or first) + 1))
    return cpus


def l3_groups():
    """Primary cores (one thread per core) per L3 cache, over the cores this process may use."""
    allowed, groups = os.sched_getaffinity(0), {}
    for path in glob.glob('/sys/devices/system/cpu/cpu[0-9]*'):
        cpu = int(path.rsplit('cpu', 1)[1])
        try:
            if cpu not in allowed or cpu != min(cpu_list(read(f'{path}/topology/thread_siblings_list'))):
                continue
            l3 = next((read(f'{d}/shared_cpu_list') for d in glob.glob(f'{path}/cache/index*')
                       if read(f'{d}/level') == '3'), 'all')
        except OSError:
            continue
        groups.setdefault(l3, []).append(cpu)
    return sorted((sorted(cpus) for cpus in groups.values()), key=lambda g: g[0])


def choose_cores(a):
    """The primary cores of one L3, away from CPU 0's (where interrupts tend to land) when
    there is another."""
    if a.cores:
        return sorted(cpu_list(a.cores))
    groups = l3_groups()
    if not groups:
        return sorted(os.sched_getaffinity(0))
    return next((g for g in groups if 0 not in g), groups[0])


def bench_env(unit, home):
    env = {k: v for k, v in os.environ.items() if not k.startswith(FOREIGN_ENV)}
    # What `cargo bench` sets for the executables it runs.
    env.update(CARGO_MANIFEST_DIR=unit['cwd'], CRITERION_HOME=home)
    return env


def criterion_args(a):
    # Only criterion's point estimates are read, so its bootstrap needs few resamples.
    args = ['--bench', '--noplot', '--warm-up-time', str(a.warm_up_time),
            '--measurement-time', str(a.measurement_time), '--nresamples', str(a.nresamples)]
    args += ['--sample-size', str(a.sample_size)]
    return args + a.criterion_args + ([a.filter] if a.filter else [])


def count_benchmarks(exe, unit, a, out):
    p = subprocess.run([exe, '--bench', '--list'] + ([a.filter] if a.filter else []), cwd=unit['cwd'],
                       env=bench_env(unit, os.path.join(out, 'list')), capture_output=True, text=True)
    return sum(line.endswith(': benchmark') for line in p.stdout.splitlines())


def results(home, statistic):
    """{benchmark id: (value, samples taken)} for one criterion run."""
    found = {}
    for path in glob.glob(os.path.join(home, '**', 'new', 'benchmark.json'), recursive=True):
        with open(path) as f:
            bench = json.load(f)
        with open(os.path.join(os.path.dirname(path), 'estimates.json')) as f:
            estimates = json.load(f)
        with open(os.path.join(os.path.dirname(path), 'sample.json')) as f:
            samples = len(json.load(f)['iters'])
        estimate = estimates.get(statistic) or estimates['mean']
        found[bench['full_id']] = (estimate['point_estimate'], samples)
    return found


def run_unit(t, unit, key, seed, rnd, a, cores, out, runs_log):
    exe = os.path.join(out, 'exe', f't{t}-s{seed}-{os.path.basename(unit["exe"])}')
    os.makedirs(os.path.dirname(exe), exist_ok=True)
    # Each tree gets layouts of its own: a tree passed twice would otherwise be linked into
    # identical binaries, and the noise it shows would leave out the layout's.
    relink(unit, None if a.no_randomize else 1000 * t + seed, exe)
    home = os.path.join(out, 'criterion', f'r{rnd}', f's{seed}', f't{t}', *key)
    print(f'--- t{t} seed {seed} round {rnd}: {key[0]}/{key[1]}', file=runs_log, flush=True)
    p = subprocess.run([exe, *criterion_args(a)], cwd=unit['cwd'], env=bench_env(unit, home),
                       stdout=runs_log, stderr=subprocess.STDOUT,
                       preexec_fn=lambda: os.sched_setaffinity(0, cores))
    os.remove(exe)
    found = results(home, a.statistic)
    if p.returncode:
        log(f'\nwarning: t{t} {key[0]}/{key[1]} seed {seed} exited with {p.returncode}; see {runs_log.name}')
    return [{'tree': t, 'seed': seed, 'round': rnd, 'package': key[0], 'target': key[1],
             'id': bench_id, 'value': value, 'samples': samples}
            for bench_id, (value, samples) in found.items()]


# Report --------------------------------------------------------------------------------------

def tcrit(dof):
    return next((t for d, t in reversed(T975) if dof >= d), T975[0][1]) if dof < 120 else 1.96


def trimmed(x, cut=0.2):
    """(trimmed mean, squared standard error, values kept) of `x` trimmed by `cut` at each end."""
    x = sorted(x)
    n = len(x)
    g = int(cut * n)
    h = n - 2 * g
    winsorized = [x[g]] * g + x[g:n - g] + [x[n - g - 1]] * g
    return st.mean(x[g:n - g]), (n - 1) * st.variance(winsorized) / (h * (h - 1)), h


def yuen(base, other):
    """(difference of 20% trimmed means, half width of its 95% interval): Yuen's test, Welch's
    on trimmed means. Interference from other processes only ever slows a run, and a few such
    runs widen Welch's interval several times over; trimming drops them."""
    ma, da, ha = trimmed(base)
    mb, db, hb = trimmed(other)
    se = math.sqrt(da + db)
    dof = (da + db) ** 2 / (da ** 2 / (ha - 1) + db ** 2 / (hb - 1)) if se else 1e9
    return mb - ma, tcrit(dof) * se


def fmt(value, bench_id):
    if 'allocated_bytes' in bench_id:
        return f'{value:9.0f} B '
    for unit, scale in (('s ', 1e9), ('ms', 1e6), ('µs', 1e3)):
        if value >= scale:
            return f'{value / scale:9.3f} {unit}'
    return f'{value:9.2f} ns'


def report(rows, names, sample_size):
    lines, verdicts = [], {'faster': 0, 'slower': 0, 'no difference': 0}
    width = max(len(n) for n in names)
    benches = dict.fromkeys((r['package'], r['target'], r['id']) for r in rows)
    for pkg, target, bench_id in benches:
        mine = [r for r in rows if (r['package'], r['target'], r['id']) == (pkg, target, bench_id)]
        per = {}
        for t in range(len(names)):
            seeds = sorted({r['seed'] for r in mine if r['tree'] == t})
            per[t] = [st.mean(r['value'] for r in mine if (r['tree'], r['seed']) == (t, s)) for s in seeds]
        lines.append(f'{pkg}/{target}: {bench_id}')
        base = per[0]
        for t, name in enumerate(names):
            v = per[t]
            if not v:
                lines.append(f'  {name:<{width}}  (absent)')
                continue
            mean = trimmed(v)[0]
            sd = f'sd {100 * st.stdev(v) / mean:4.1f}%' if len(v) > 1 and mean else 'sd    -'
            line = f'  {name:<{width}}  {fmt(mean, bench_id)}  {sd}  n={len(v)}'
            if t and len(v) > 1 and len(base) > 1 and trimmed(base)[0]:
                d, ci = yuen(base, v)
                verdict = 'faster' if d + ci < 0 else 'slower' if d - ci > 0 else 'no difference'
                verdicts[verdict] += 1
                ref = trimmed(base)[0]
                line += f'   {100 * d / ref:+6.1f}% ± {100 * ci / ref:4.1f}% -> {verdict}'
            lines.append(line)
    # A benchmark group's own settings (sample_size, measurement_time...) win over criterion's
    # command line, so those benchmarks ran their way, taking longer than estimated.
    own = sorted({r['id'] for r in rows if r.get('samples', sample_size) != sample_size})
    if own:
        lines.append('')
        lines.append(f'{len(own)} benchmarks set their own criterion settings in code and ran with '
                     f'those rather than --sample-size {sample_size} and the times given, e.g. {own[0]}')
    compared = sum(verdicts.values())
    if compared:
        lines.append('')
        lines.append(f'{compared} comparisons: {verdicts["slower"]} slower, {verdicts["faster"]} faster, '
                     f'{verdicts["no difference"]} no difference. At 95%, about {0.05 * compared:.1f} '
                     'of them would be flagged by chance alone; pass one tree twice to see this '
                     "machine's noise.")
    return '\n'.join(lines)


# Main ----------------------------------------------------------------------------------------

def parse_args():
    argv = sys.argv[1:]
    extra = []
    if '--' in argv:
        extra, argv = argv[argv.index('--') + 1:], argv[:argv.index('--')]
    ap = argparse.ArgumentParser(
        usage='%(prog)s [options] TREE TREE... [-- CRITERION_ARGS]', description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('trees', nargs='+', metavar='TREE',
                    help='a directory (uncommitted files included) or a git revision; the first is '
                         'the baseline. Pass one twice to see the noise')
    ap.add_argument('-p', '--package', action='append', default=[],
                    help='crate to benchmark (repeatable; default: every crate with benchmarks)')
    ap.add_argument('--bench', action='append', default=[], help='bench target (repeatable)')
    ap.add_argument('--filter', help='criterion benchmark filter (a regex over benchmark ids)')
    # Defaults from an ablation on libdd-ddsketch and libdd-trace-normalization (see
    # benchmark/README.md): the spread between layouts dominates, so many short runs of
    # distinct layouts beat few long ones.
    ap.add_argument('--seeds', type=int, default=16, help='layouts per tree (default 16)')
    ap.add_argument('--rounds', type=int, default=1, help='runs per layout (default 1)')
    ap.add_argument('--warm-up-time', type=float, default=0.3, help='criterion warm-up, s (default 0.3)')
    ap.add_argument('--measurement-time', type=float, default=0.25,
                    help='criterion measurement, s (default 0.25)')
    ap.add_argument('--nresamples', type=int, default=1000,
                    help="criterion bootstrap resamples (default 1000; only point estimates are used)")
    ap.add_argument('--sample-size', type=int, default=100,
                    help="criterion samples (default 100, criterion's)")
    ap.add_argument('--statistic', choices=('median', 'mean', 'slope'), default='median',
                    help="criterion estimate taken from each run (default median)")
    ap.add_argument('--no-randomize', action='store_true',
                    help="link every seed as rustc does, so seeds are plain repeats of one layout "
                         "(the control: what measuring one build per tree sees)")
    ap.add_argument('--cores', help='cores to run on (default: the primary cores of one L3)')
    ap.add_argument('--out', help='results (default target/stable-benchmarks/<time>)')
    ap.add_argument('--yes', action='store_true', help="don't ask before a long run")
    a = ap.parse_args(argv)
    a.criterion_args = extra
    if a.seeds < 2:
        ap.error('--seeds must be at least 2 to estimate the layout noise')
    return a


def main():
    a = parse_args()
    if not sys.platform.startswith('linux'):
        die('Linux only: layouts are randomized by lld for ELF')
    out = os.path.abspath(a.out or os.path.join(REPO, 'target', 'stable-benchmarks',
                                                time.strftime('%Y%m%d-%H%M%S')))
    os.makedirs(out, exist_ok=True)
    log(f'results in {out}')

    names, builds = [], []
    for i, tree in enumerate(a.trees):
        names.append(tree_name(tree))
        log(f't{i}: building {names[-1]}')
        builds.append(build(i, tree, a, out))
    keys = sorted({k for b in builds for k in b})
    if not keys:
        die('no bench targets selected')

    cores = choose_cores(a)
    total, runs = 0, 0
    for key in keys:
        t, unit = next((t, b[key]) for t, b in enumerate(builds) if key in b)
        exe = os.path.join(out, 'exe', f'list-{os.path.basename(unit["exe"])}')
        os.makedirs(os.path.dirname(exe), exist_ok=True)
        relink(unit, 1, exe)  # also checks that the linker takes the padding flag
        count = count_benchmarks(exe, unit, a, out)
        os.remove(exe)
        present = sum(key in b for b in builds)
        total += count * present * a.seeds * a.rounds
        runs += present * a.seeds * a.rounds
    # Measured: about 0.06 s per benchmark beyond warm-up and measurement, and well under a
    # second per executable to link and start.
    est = total * (a.warm_up_time + a.measurement_time + 0.1) + runs
    log(f'cores {",".join(map(str, cores))}; {len(keys)} bench targets, {total} benchmark runs, '
        f'about {est / 60:.0f} min')
    if est > 3600 and not a.yes and sys.stdin.isatty():
        if input('continue? [y/N] ').strip().lower() != 'y':
            sys.exit(1)

    rows, done = [], 0
    with open(os.path.join(out, 'runs.log'), 'a') as runs_log, \
            open(os.path.join(out, 'rows.jsonl'), 'w') as rows_out:
        for rnd in range(1, a.rounds + 1):
            for seed in range(1, a.seeds + 1):
                # Trees alternate within each seed, and swap order from one seed to the next,
                # so drift over the run hits them alike.
                order = list(range(len(builds)))
                if seed % 2 == 0:
                    order.reverse()
                for key in keys:
                    for t in order:
                        if key not in builds[t]:
                            continue
                        for row in run_unit(t, builds[t][key], key, seed, rnd, a, cores, out, runs_log):
                            rows.append(row)
                            rows_out.write(json.dumps(row) + '\n')
                        rows_out.flush()
                        done += 1
                        if sys.stderr.isatty():
                            print(f'\r{done}/{runs} runs', end='', file=sys.stderr, flush=True)
    if sys.stderr.isatty():
        print(file=sys.stderr)
    if not rows:
        die(f'no criterion results; see {out}/runs.log')
    text = report(rows, names, a.sample_size)
    with open(os.path.join(out, 'report.txt'), 'w') as f:
        f.write(text + '\n')
    print(text)


if __name__ == '__main__':
    main()
