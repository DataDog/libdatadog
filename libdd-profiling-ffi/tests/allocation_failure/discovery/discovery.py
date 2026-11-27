#!/usr/bin/env python3
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

"""Scenario discovery for the allocation-failure harness: branch coverage and fuzzing.

Coverage and fuzzing builds only find scenarios. Ordinals are never taken from them: the
harness enumerates every scenario again in the normal and release builds.

Commands (run in this order; every output goes under --work, nothing under /tmp):

  coverage        Build the harness with LLVM source-based branch coverage (nightly), run it
                  with one profile file per worker, merge the profiles per scenario.
  fuzz            Build and run the `profile_sequence` fuzz target (allocation failure
                  disabled), then minimize the corpus.
  fuzz-coverage   Branch coverage of every minimized fuzz input, one profile per input.
  promote         Fuzz inputs that reach owned branches no scenario baseline reaches become
                  named scenarios in `promoted.rs`. Then run `coverage` again.
  manifest        Write `SCENARIOS.md`: scenario, inputs, branches it reaches, and the
                  owned allocation-relevant branches that no worker reached (a gap report).

Owned code: the files of this repository outside tests, benches, examples, fuzz and build
scripts. Branches come from `llvm-cov export -format=lcov` (BRDA records).
"""

import argparse
import concurrent.futures
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
HARNESS = HERE.parent
REPO = HARNESS.parents[2]
FUZZ = REPO / "libdd-profiling-ffi" / "fuzz"
FUZZ_TARGET = "profile_sequence"


def nightly():
    text = (REPO / "ci-toolchain.toml").read_text()
    return re.search(r'^nightly = "([^"]+)"', text, re.M).group(1)


NIGHTLY = nightly()
COVERAGE_VARS = (
    "LLVM_PROFILE_FILE",
    "RUSTC_WRAPPER",
    "RUSTFLAGS",
    "CARGO_LLVM_COV",
    "CARGO_LLVM_COV_SHOW_ENV",
    "CARGO_LLVM_COV_TARGET_DIR",
    "CARGO_LLVM_COV_BUILD_DIR",
    "__CARGO_LLVM_COV_RUSTC_WRAPPER",
    "__CARGO_LLVM_COV_RUSTC_WRAPPER_RUSTFLAGS",
    "__CARGO_LLVM_COV_RUSTC_WRAPPER_CRATE_NAMES",
    "LIBDD_ALLOCATION_CHAOS_TEST",
)
# Not owned: dependencies, the standard library, tests and tools of this repository.
IGNORE_FILES = r"(/\.cargo/|/rustc/|/\.rustup/|/tests/|/benches/|/examples/|/fuzz/|/build\.rs$|/build/)"
# A branch is allocation-relevant when its line, or one of the two next lines, allocates or
# handles an allocation result.
ALLOC_RE = re.compile(
    r"try_reserve|reserve\(|with_capacity|try_with_capacity|\.push\(|\.insert\(|extend|to_vec"
    r"|to_owned|to_string|into_owned|String::|Vec::|vec!|Box::|format!|collect|\.clone\(\)"
    r"|intern|alloc|AllocError|TryReserveError|OutOfMemory|grow|Arc::new|Rc::new"
)


def log(msg):
    print(f"[discovery] {msg}", file=sys.stderr, flush=True)


def base_env(work):
    env = {k: v for k, v in os.environ.items() if k not in COVERAGE_VARS}
    env["TMPDIR"] = str(work / "tmp")
    (work / "tmp").mkdir(parents=True, exist_ok=True)
    # Instrumented build scripts and one-off runs otherwise write `default_*.profraw` in their
    # working directory, inside the repository.
    env["LLVM_PROFILE_FILE"] = str(work / "tmp" / "profraw" / "%m_%p.profraw")
    return env


def run(cmd, env, cwd=REPO, capture=False, check=True):
    log(" ".join(str(c) for c in cmd))
    result = subprocess.run(
        [str(c) for c in cmd],
        cwd=cwd,
        env=env,
        text=True,
        stdout=subprocess.PIPE if capture else None,
    )
    if check and result.returncode != 0:
        sys.exit(f"command failed ({result.returncode}): {cmd[0]}")
    return result


def llvm(tool):
    sysroot = subprocess.run(
        ["rustc", f"+{NIGHTLY}", "--print", "sysroot"], text=True, capture_output=True, check=True
    ).stdout.strip()
    return Path(sysroot) / "lib" / "rustlib" / host() / "bin" / tool


def host():
    out = subprocess.run(["rustc", "-vV"], text=True, capture_output=True, check=True).stdout
    return re.search(r"^host: (\S+)", out, re.M).group(1)


def cargo_executable(cmd, env, cwd, name):
    """Runs a cargo build with JSON messages and returns the executable of `name`."""
    out = run(cmd + ["--message-format=json"], env, cwd=cwd, capture=True).stdout
    exe = None
    for line in out.splitlines():
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (
            msg.get("reason") == "compiler-artifact"
            and msg.get("target", {}).get("name") == name
            and msg.get("executable")
        ):
            exe = msg["executable"]
    if exe is None:
        sys.exit(f"no executable for {name}")
    if b"__llvm_profile_runtime" not in Path(exe).read_bytes():
        sys.exit(f"{exe} is not instrumented: its target directory holds a stale build")
    return Path(exe)


# ---------------------------------------------------------------------------------------------
# Profiles and lcov
# ---------------------------------------------------------------------------------------------


def merge(files, out, work):
    out.parent.mkdir(parents=True, exist_ok=True)
    listing = work / "tmp" / f"{out.name}.inputs"
    listing.write_text("\n".join(str(f) for f in files) + "\n")
    subprocess.run(
        [llvm("llvm-profdata"), "merge", "-sparse", f"-input-files={listing}", "-o", out],
        check=True,
    )


def export_lcov(exe, profdata, out):
    out.parent.mkdir(parents=True, exist_ok=True)
    with open(out, "w") as f:
        subprocess.run(
            [
                llvm("llvm-cov"),
                "export",
                "-format=lcov",
                f"-instr-profile={profdata}",
                f"-ignore-filename-regex={IGNORE_FILES}",
                exe,
            ],
            stdout=f,
            check=True,
        )


def parse_lcov(path):
    """`{file: {"br": {(line, block, branch): taken}, "fn": {name: (line, hits)}}}`.

    `taken` is None when the branch's block never ran (`-`). Instantiations are summed.
    """
    files = {}
    current = None
    prefix = str(REPO) + "/"
    for line in Path(path).read_text().splitlines():
        if line.startswith("SF:"):
            name = line[3:]
            name = name[len(prefix) :] if name.startswith(prefix) else name
            current = files.setdefault(name, {"br": {}, "fn": {}, "fnline": {}})
        elif line.startswith("BRDA:") and current is not None:
            ln, block, branch, taken = line[5:].split(",")
            key = (int(ln), int(block), int(branch))
            count = None if taken == "-" else int(taken)
            prev = current["br"].get(key)
            if count is not None or prev is None:
                current["br"][key] = (prev or 0) + count if count is not None else prev
        elif line.startswith("FN:") and current is not None:
            ln, name = line[3:].split(",", 1)
            current["fnline"][name] = int(ln)
        elif line.startswith("FNDA:") and current is not None:
            hits, name = line[5:].split(",", 1)
            current["fn"][name] = current["fn"].get(name, 0) + int(hits)
    return files


def covered(lcov):
    """Branch outcomes taken at least once: `{(file, line, block, branch)}`."""
    return {
        (f, *key)
        for f, data in lcov.items()
        for key, taken in data["br"].items()
        if taken
    }


def comparable(branches):
    """Keys that compare across binaries: the block numbering can differ, the line cannot."""
    return {(f, line, branch) for f, line, _block, branch in branches}


# ---------------------------------------------------------------------------------------------
# coverage
# ---------------------------------------------------------------------------------------------


def coverage_env(work):
    env = base_env(work)
    # Cargo does not fingerprint the cargo-llvm-cov wrapper flags: this directory must only ever
    # hold instrumented builds.
    target = work / "llvm-cov-target"
    env["CARGO_TARGET_DIR"] = str(target)
    out = run(
        ["cargo", f"+{NIGHTLY}", "llvm-cov", "show-env", "--branch", "--sh"],
        env,
        capture=True,
    ).stdout
    for line in out.splitlines():
        m = re.match(r"export (\w+)=(.*)$", line)
        if m:
            value = m.group(2)
            if len(value) >= 2 and value[0] == value[-1] == "'":
                value = value[1:-1].replace("'\\''", "'")
            env[m.group(1)] = value
    env["LLVM_PROFILE_FILE"] = base_env(work)["LLVM_PROFILE_FILE"]
    env["CARGO_TARGET_DIR"] = str(target)
    env["CARGO_LLVM_COV_TARGET_DIR"] = str(target)
    env["CARGO_LLVM_COV_BUILD_DIR"] = str(target)
    return env


def build_coverage_harness(work):
    env = coverage_env(work)
    cmd = ["cargo", f"+{NIGHTLY}", "build", "--offline", "-p", "libdd-profiling-ffi"]
    cmd += ["--test", "allocation_failure"]
    return cargo_executable(cmd, env, REPO, "allocation_failure"), env


def latest_run(work):
    runs = sorted((work / "runs").glob("*"))
    if not runs:
        sys.exit("no coverage run: run `coverage` first")
    return runs[-1]


def cmd_coverage(args):
    work = args.work
    exe, env = build_coverage_harness(work)
    run_dir = work / "runs" / time.strftime("%Y%m%d-%H%M%S")
    profraw = run_dir / "profraw"
    profraw.mkdir(parents=True)
    env = base_env(work)
    env["ALLOC_CHAOS_PROFILE_DIR"] = str(profraw)
    env["LLVM_PROFILE_FILE"] = str(profraw / "controller-%p.profraw")
    result = run([exe, *args.filters], env, check=False)
    (run_dir / "exe").write_text(str(exe) + "\n")
    names = run([exe, "__scenarios"], env, capture=True).stdout
    (run_dir / "scenarios.tsv").write_text(names)
    if result.returncode != 0:
        log(f"harness exit status {result.returncode}: see its output above")
    process_run(run_dir, exe, work)
    log(f"coverage run: {run_dir}")
    if result.returncode != 0:
        sys.exit(result.returncode)


def process_run(run_dir, exe, work):
    profraw = run_dir / "profraw"
    jobs = []
    for scenario_dir in sorted(p for p in profraw.iterdir() if p.is_dir()):
        files = sorted(scenario_dir.glob("*.profraw"))
        baseline = [f for f in files if f.name.startswith("baseline-")]
        if not baseline:
            continue
        jobs.append((scenario_dir.name, "baseline", baseline))
        jobs.append((scenario_dir.name, "all", files))

    def one(job):
        scenario, kind, files = job
        profdata = run_dir / "profdata" / f"{scenario}.{kind}.profdata"
        merge(files, profdata, work)
        export_lcov(exe, profdata, run_dir / "lcov" / f"{scenario}.{kind}.lcov")
        return len(files)

    with concurrent.futures.ThreadPoolExecutor(os.cpu_count() or 4) as pool:
        counts = list(pool.map(one, jobs))
    every = sorted(profraw.glob("*/*.profraw"))
    merge(every, run_dir / "profdata" / "merged.profdata", work)
    export_lcov(exe, run_dir / "profdata" / "merged.profdata", run_dir / "lcov" / "merged.lcov")
    log(f"{len(every)} worker profiles, {len(jobs) // 2} scenarios, {sum(counts)} merged")


# ---------------------------------------------------------------------------------------------
# fuzz
# ---------------------------------------------------------------------------------------------


def fuzz_paths(work):
    root = work / "fuzz"
    for sub in ("corpus", "min", "artifacts"):
        (root / sub).mkdir(parents=True, exist_ok=True)
    return root


def cmd_fuzz(args):
    work = args.work
    root = fuzz_paths(work)
    env = base_env(work)
    env["CARGO_TARGET_DIR"] = str(work / "fuzz-target")
    run(["cargo", f"+{NIGHTLY}", "fuzz", "build", FUZZ_TARGET], env, cwd=FUZZ)
    exe = work / "fuzz-target" / host() / "release" / FUZZ_TARGET
    corpora = [root / "corpus"]
    run(
        [
            exe,
            *corpora,
            f"-artifact_prefix={root / 'artifacts'}/",
            f"-max_total_time={args.seconds}",
            f"-fork={args.jobs}",
            "-max_len=256",
            "-timeout=30",
            "-rss_limit_mb=4096",
            "-print_final_stats=1",
        ],
        env,
        cwd=work,
    )
    run([exe, "-merge=1", root / "min", *corpora], env, cwd=work)
    log(f"minimized corpus: {len(list((root / 'min').iterdir()))} inputs")


def cmd_fuzz_coverage(args):
    work = args.work
    root = fuzz_paths(work)
    env = base_env(work)
    env["CARGO_TARGET_DIR"] = str(work / "fuzz-cov-target")
    env["RUSTFLAGS"] = "-Cinstrument-coverage -Zcoverage-options=branch --cfg fuzzing"
    cmd = ["cargo", f"+{NIGHTLY}", "build", "--bin", FUZZ_TARGET]
    exe = cargo_executable(cmd, env, FUZZ, FUZZ_TARGET)
    out = root / "coverage"
    inputs = sorted((root / "min").iterdir())

    def one(path):
        lcov = out / "lcov" / f"{path.name}.lcov"
        if lcov.exists():
            return
        raw = out / "profraw" / f"{path.name}.profraw"
        raw.parent.mkdir(parents=True, exist_ok=True)
        run_env = dict(env)
        run_env["LLVM_PROFILE_FILE"] = str(raw)
        subprocess.run(
            [exe, path], env=run_env, cwd=work, check=True,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        profdata = out / "profdata" / f"{path.name}.profdata"
        merge([raw], profdata, work)
        export_lcov(exe, profdata, lcov)
        # Every dependency is instrumented here, so raw profiles are large: keep the merged one.
        raw.write_bytes(b"")

    with concurrent.futures.ThreadPoolExecutor(os.cpu_count() or 4) as pool:
        list(pool.map(one, inputs))
    (out / "exe").write_text(str(exe) + "\n")
    log(f"{len(inputs)} fuzz inputs covered: {out / 'lcov'}")


# ---------------------------------------------------------------------------------------------
# promote
# ---------------------------------------------------------------------------------------------


def baseline_union(run_dir, skip_prefix=None):
    union = set()
    for lcov in sorted((run_dir / "lcov").glob("*.baseline.lcov")):
        scenario = lcov.name.split(".")[0]
        if skip_prefix and scenario.startswith(skip_prefix):
            continue
        union |= comparable(covered(parse_lcov(lcov)))
    return union


def cmd_promote(args):
    work = args.work
    run_dir = latest_run(work)
    exe = Path((run_dir / "exe").read_text().strip())
    # `promoted.rs` is rewritten whole: the previous promoted scenarios do not count.
    union = baseline_union(run_dir, skip_prefix="fuzz_seq_")
    log(f"non-promoted scenario baselines reach {len(union)} owned branch outcomes")
    root = fuzz_paths(work)
    candidates = {}
    for lcov in sorted((root / "coverage" / "lcov").glob("*.lcov")):
        candidates[lcov.stem] = comparable(covered(parse_lcov(lcov)))
    chosen = []
    while len(chosen) < args.max and candidates:
        name, branches = max(
            candidates.items(), key=lambda kv: (len(kv[1] - union), -len(kv[1]), kv[0])
        )
        new = branches - union
        if len(new) < args.min_new:
            break
        chosen.append((name, sorted(new)))
        union |= branches
        del candidates[name]
    env = base_env(work)
    entries = []
    for index, (name, new) in enumerate(chosen, start=1):
        data = (root / "min" / name).read_bytes()
        describe = run(
            [exe, "__sequence", "describe", root / "min" / name], env, capture=True
        ).stdout.strip()
        replay = run(
            [exe, "__sequence", "replay", root / "min" / name], env, capture=True
        ).stdout.strip()
        if replay != "result Ok(()), check Ok(())":
            log(f"skipping {name}: replay gave {replay}")
            continue
        files = sorted({f for f, _, _ in new})
        entries.append((f"fuzz_seq_{index:02}", describe, data, len(new), files))
        log(f"promote {name}: {len(new)} new owned branch outcomes in {', '.join(files)}")
    write_promoted(entries)


def write_promoted(entries):
    lines = [
        "// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/",
        "// SPDX-License-Identifier: Apache-2.0",
        "",
        "//! Generated by `discovery/discovery.py promote`. Do not edit: run it again.",
        "//!",
        "//! Fuzz inputs whose baseline reached owned branch outcomes that no scenario baseline",
        "//! reached when they were promoted. Each line is the decoded call sequence.",
        "",
        "crate::fuzzed::promoted! {",
    ]
    for name, describe, data, new, files in entries:
        noun = "outcome" if new == 1 else "outcomes"
        lines.append(f"    // {new} new owned branch {noun} in:")
        lines.extend(f"    // - {f}" for f in files)
        text = describe.replace("\\", "\\\\").replace('"', '\\"')
        lines.append(f"    {name},")
        lines.append(f'    "{text}",')
        hexes = [f"0x{b:02x}" for b in data]
        lines.append("    [")
        for i in range(0, len(hexes), 12):
            lines.append("        " + ", ".join(hexes[i : i + 12]) + ",")
        lines.append("    ];")
    lines.append("}")
    (HARNESS / "promoted.rs").write_text("\n".join(lines) + "\n")
    log(f"wrote {HARNESS / 'promoted.rs'} with {len(entries)} scenarios")


# ---------------------------------------------------------------------------------------------
# manifest
# ---------------------------------------------------------------------------------------------


def source_lines(file, cache={}):
    if file not in cache:
        try:
            cache[file] = (REPO / file).read_text().splitlines()
        except OSError:
            cache[file] = []
    return cache[file]


def allocation_relevant(file, line):
    src = source_lines(file)
    window = " ".join(src[line - 1 : line + 2])
    return bool(ALLOC_RE.search(window))


def line_ranges(lines):
    out = []
    for ln in sorted(lines):
        if out and ln == out[-1][1] + 1:
            out[-1][1] = ln
        else:
            out.append([ln, ln])
    return ", ".join(str(a) if a == b else f"{a}-{b}" for a, b in out)


def cmd_manifest(args):
    run_dir = latest_run(args.work)
    inputs = {}
    for raw in (run_dir / "scenarios.tsv").read_text().splitlines():
        name, _, text = raw.partition("\t")
        inputs[name] = text
    baselines, alls = {}, {}
    for lcov in sorted((run_dir / "lcov").glob("*.lcov")):
        scenario, kind = lcov.name.split(".")[:2]
        if kind == "baseline":
            baselines[scenario] = covered(parse_lcov(lcov))
        elif kind == "all":
            alls[scenario] = covered(parse_lcov(lcov))
    merged = parse_lcov(run_dir / "lcov" / "merged.lcov")
    reached = covered(merged)
    base_union = set().union(*baselines.values()) if baselines else set()
    out = []
    out.append("# Allocation-failure scenario manifest")
    out.append("")
    out.append("Generated by `discovery/discovery.py manifest`. Do not edit: run it again.")
    out.append("")
    out.append(
        "Branch coverage of owned code (this repository, outside tests, benches, examples, fuzz"
        f" and build scripts), from a `{NIGHTLY}` build with `-Zcoverage-options=branch`. One"
        " LLVM profile per worker, merged per scenario. A worker that aborts writes no profile,"
        " so the \"with failures\" column counts the injected runs that returned. Coverage"
        " builds only find scenarios: the harness enumerates ordinals again in the normal and"
        " release builds."
    )
    out.append("")
    instrumented = sum(len(d["br"]) for d in merged.values())
    out.append(f"- Owned branch outcomes in the files the harness reaches: {instrumented}")
    out.append(f"- Reached by a scenario baseline: {len(base_union)}")
    out.append(f"- Reached by any worker, injected failures included: {len(reached)}")
    out.append("")
    out.append("## Scenarios")
    out.append("")
    out.append(
        "Branch outcomes: owned branch outcomes the baseline reaches; with failures: also"
        " reached by the injected runs that returned; only here: reached by no other baseline."
    )
    out.append("")
    out.append("| Scenario | Inputs | Branch outcomes | With failures | Only here |")
    out.append("|---|---|---:|---:|---:|")
    order = [n for n in inputs if n in baselines]
    for name in order:
        others = set().union(*(b for n, b in baselines.items() if n != name))
        only = baselines[name] - others
        text = inputs[name].replace("|", "\\|")
        out.append(
            f"| `{name}` | {text} | {len(baselines[name])} | {len(alls.get(name, set()))}"
            f" | {len(only)} |"
        )
    missing = [n for n in inputs if n not in baselines]
    if missing:
        out.append("")
        out.append(
            "No baseline profile (the scenario did not run in this coverage run): "
            + ", ".join(f"`{n}`" for n in missing)
        )
    out.append("")
    out.append("## Branches each scenario reaches")
    out.append("")
    out.append("Lines with at least one owned branch outcome reached by the baseline.")
    for name in order:
        by_file = {}
        for f, line, _b, _r in baselines[name]:
            by_file.setdefault(f, set()).add(line)
        if not by_file:
            continue
        out.append("")
        out.append(f"<details><summary><code>{name}</code></summary>")
        out.append("")
        for f in sorted(by_file):
            out.append(f"- `{f}`: {line_ranges(by_file[f])}")
        out.append("")
        out.append("</details>")
    rows = []
    for f, data in sorted(merged.items()):
        by_branch = {}
        for (line, block, branch), taken in data["br"].items():
            by_branch.setdefault((line, block), {})[branch] = taken
        for (line, block), outcomes in sorted(by_branch.items()):
            evaluated = any(t for t in outcomes.values())
            for branch, taken in sorted(outcomes.items()):
                if taken:
                    continue
                if not allocation_relevant(f, line):
                    continue
                kind = "outcome never taken" if evaluated else "condition never evaluated"
                rows.append((f, line, branch, kind))
    out.append("")
    out.append("## Uncovered allocation-relevant branches")
    out.append("")
    out.append(
        "Owned branch outcomes that no worker reached, injected failures included, on a line"
        " that allocates or handles an allocation result (the line or the two next ones match"
        f" `{ALLOC_RE.pattern}`). This list is a coverage gap report: nothing in it is accepted"
        " or excused."
    )
    out.append("")
    out.append("| File | Line | Outcome | Kind |")
    out.append("|---|---:|---:|---|")
    for f, line, branch, kind in rows:
        out.append(f"| `{f}` | {line} | {branch} | {kind} |")
    (HARNESS / "SCENARIOS.md").write_text("\n".join(out) + "\n")
    log(
        f"wrote SCENARIOS.md: {len(order)} scenarios, {len(rows)} uncovered allocation-relevant"
        " outcomes"
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    parser.add_argument(
        "--work",
        type=Path,
        default=Path(os.environ.get("ALLOC_CHAOS_WORK", REPO / "target" / "alloc-chaos-discovery")),
        help="output folder (default: $ALLOC_CHAOS_WORK or target/alloc-chaos-discovery)",
    )
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("coverage")
    p.add_argument("filters", nargs="*", help="harness test filters (default: every test)")
    p.set_defaults(f=cmd_coverage)
    p = sub.add_parser("fuzz")
    p.add_argument("--seconds", type=int, default=600)
    p.add_argument("--jobs", type=int, default=max(1, (os.cpu_count() or 2) // 2))
    p.set_defaults(f=cmd_fuzz)
    sub.add_parser("fuzz-coverage").set_defaults(f=cmd_fuzz_coverage)
    p = sub.add_parser("promote")
    p.add_argument("--max", type=int, default=8, help="most inputs promoted")
    p.add_argument("--min-new", type=int, default=2, help="fewest new outcomes to promote")
    p.set_defaults(f=cmd_promote)
    sub.add_parser("manifest").set_defaults(f=cmd_manifest)
    args = parser.parse_args()
    args.work = args.work.resolve()
    args.f(args)


if __name__ == "__main__":
    main()
