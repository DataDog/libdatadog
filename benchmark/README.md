# Benchmarks

- `run_benchmarks_ci.sh` and `select_bench_packages.sh` run the criterion benchmarks in CI,
  through the benchmarking platform (`.gitlab/benchmarks.yml`).
- `bench_features.sh` lists the features each crate's benchmarks need. Both CI and
  `stable-benchmarks.py` read it.
- `stable-benchmarks.py` compares benchmarks between trees, the way described below.

## Stable comparisons

`benchmark/stable-benchmarks.py` compares criterion benchmarks between builds without the
bias that code and data layout put into single measurements.

### Why

Where the linker happens to place functions and data changes how they share instruction
caches, branch predictors, and cache lines. An edit anywhere in a crate, or in anything
linked with it, moves everything after it. So a single build measured against another
mostly compares two layouts.

The `libdd-ddsketch` and `libdd-trace-normalization` benchmarks were measured with the same
code in 8 layouts, 3 runs each, on a Zen 2 EPYC 7R32. 8 of the 33 benchmarks depend heavily
on layout:

- `ddsketch_encode/encode_to_vec/collapsing` varies 11.7% between layouts (CoV) and 1.0%
  between runs of one layout. Its fastest and slowest layouts are 26% apart.
- `normalize_service/...Weird...`, `normalize_name/bad-name` and `normalize_service/test_ASCII`
  are 22–29% apart from fastest to slowest layout.

The other benchmarks hardly depend on layout. When two builds of identical code are compared
the way a single-build run compares them (3 runs each, Welch at 95%), 17% of them come out
"different" on average, and 43–64% on the layout-sensitive benchmarks. The same data
compared over layouts flags 1%.

Run-time placement adds to this. ASLR moves the stack, heap and mapping bases on every run,
and a few benchmarks switch between two speeds from one process to the next. For example,
`ddsketch_encode/into_pb/mixed` measures 25.9 or 28.5 ns, in the same binary. Each layout
is a fresh process, so the spread over seeds covers this as well.

The script therefore measures every tree in several layouts and compares the averages.
Each tree is compiled once. Every bench executable is then linked again once per seed with
lld's `--randomize-section-padding`, which pads between functions and data objects (rustc
puts each in its own section) and at the start of each segment. This is the link-time half
of [Stabilizer](https://github.com/ccurtsinger/stabilizer)'s approach, without its runtime
re-randomization. It comes from dd-trace-c's `bench/stable-benchmarks.sh`.

### Usage

```sh
benchmark/stable-benchmarks.py main . -p libdd-trace-utils          # main against the working tree
benchmark/stable-benchmarks.py HEAD~1 HEAD -p libdd-ddsketch         # two revisions
benchmark/stable-benchmarks.py . . -p libdd-ddsketch                 # the noise: a tree against itself
benchmark/stable-benchmarks.py main . -p libdd-trace-utils --bench vec_map_bench --filter 'get'
```

A tree is a directory (its uncommitted files included) or a git revision. The first tree is
the baseline. Options:

- `-p` / `--package`: the crates to benchmark. Repeat it for several crates. The default
  is every crate with a bench target, which takes hours.
- `--bench`: the bench targets to run.
- `--filter`: criterion's benchmark filter.
- `--seeds`: the number of layouts per tree (default 16).
- `--rounds`: the number of runs per layout (default 1).
- `--warm-up-time`, `--measurement-time`, `--sample-size` and `--nresamples`: passed to
  criterion (defaults 0.3 s, 0.25 s, 30 and 1000). These defaults are tuned for many short
  runs; see [Choosing the defaults](#choosing-the-defaults).
- `--statistic`: which criterion estimate is taken from each run: `median`, `mean`, or
  `slope` (default `median`).
- `--cores`: the cores to pin the benchmarks to.
- `--no-randomize`: link every seed as rustc does, so the seeds are just repeated runs of one
  layout. This is the control: it shows what measuring one build per tree reports.

Arguments after `--` go to criterion as they are.

Before measuring, the script prints how many benchmark runs there are and roughly how long
they take, and it asks before anything over an hour. Each criterion run costs about
`warm-up + measurement + 1` seconds, multiplied by the benchmarks, trees, seeds, and rounds.
So narrow the run with `-p`, `--bench`, and `--filter`. Results, build logs, criterion's
output, and the per-run rows go to `target/stable-benchmarks/<time>`.

### What runs

Each tree is copied (a revision is exported with `git archive`, along with the initialized
submodules) and built with `cargo bench --no-run`, using the features in
`bench_features.sh` and a target directory of its own.
`stable-link-recorder.py` stands in as the linker. It saves each executable's link command
along with the inputs that rustc deletes after linking. Before each run, the executable is
linked again with that run's seed, and it is deleted afterwards.

Every run is pinned to the primary cores (one hardware thread per core) of a single L3. That
L3 is one that does not hold CPU 0 when there is another. `DD_*` and `OTEL_*` variables are
removed from the environment. For each seed, the trees alternate benchmark by benchmark,
and their order swaps from one seed to the next. That way drift over the run, such as
thermal effects or other load on the machine, affects every tree alike.

### Reading the report

```
libdd-ddsketch/ddsketch_bench: ddsketch_encode/encode_to_vec/collapsing
  base      1.657 µs  sd  9.5%  n=16
  copy      1.666 µs  sd  8.5%  n=16     +0.5% ±  1.1% -> no difference
libdd-ddsketch/ddsketch_bench: ddsketch_add/add/collapsing
  base     93.921 µs  sd  0.4%  n=16
  copy     93.957 µs  sd  0.4%  n=16     +0.0% ±  0.4% -> no difference
```

(An A/A run: the same tree as `base` and as `copy`.)

Each line shows a tree's 20% trimmed mean over its seeds, and the standard deviation across
seeds: how much layout and noise move that benchmark. The interval is Yuen's 95% interval for
the difference from the first tree. Yuen's test is Welch's test on trimmed means.
Interference from other processes can only slow a run down. On a shared machine a few such
runs widened Welch's interval from ±1.2% to ±4.9% in one A/A run, and trimming drops them.
Only "faster" or "slower" is a finding. More seeds narrow the interval roughly as one over
the square root of their number.

A report compares many benchmarks at once, so at 95% about one in twenty benchmarks that
did not change is still flagged. The summary line says how many to expect. Passing one tree
twice shows the noise for the machine; each tree is linked with seeds of its own, so the two
copies get different layouts. Re-running only the flagged benchmarks with more seeds tells
real changes from chance.

Some benchmarks set criterion's settings in code, such as `group.sample_size(200)` or
`group.measurement_time(...)`. Criterion lets those win over its command line, so these
benchmarks run their own way and take longer. The report lists them. They are in
libdd-trace-normalization, libdd-trace-obfuscation (credit cards), libdd-trace-utils
(vec_map), libdd-profiling (profiles dictionary), libdd-data-pipeline (trace buffer) and
libdd-ffe-test-suite. CI's `--measurement-time 5 --sample-size=200` doesn't reach them either.

Allocation benchmarks (`allocated_bytes`) are deterministic. Their standard deviation is 0,
and any difference is real.

### Choosing the defaults

The defaults come from subsampling the 8 layouts × 3 runs above as A/A comparisons, with
criterion's samples truncated to stand in for shorter runs. They were then checked with
real A/A runs:

- **Layouts matter more than anything else.** The interval narrows as one over the square
  root of the number of layouts, for both the layout and the run-to-run spread. More runs of
  one layout only narrow the run-to-run part.
  For the same time, 8 layouts × 1 run gives ±0.9% on the median benchmark, and
  4 layouts × 2 runs gives ±1.05%. So `--rounds` is 1 and `--seeds` is 16.
- **Short runs suffice.** Going from 1 s of measurement to 0.1 s only moved the median
  run-to-run CoV from 0.64% to 0.82%. So warm-up is 0.3 s and measurement 0.25 s.
- **Keep 100 samples.** On quiet data, 30 samples were as precise as 100. Under load,
  though, a short run is more exposed to a burst of interference. 100 samples cost only 12%
  more on libdd-ddsketch.
- **Criterion's bootstrap only serves its own intervals.** The script reads point estimates
  only, so `--nresamples` is 1000 rather than 100,000.

With these defaults, a libdd-ddsketch benchmark costs about 0.7 s per run, against about 5 s
with criterion's usual settings. 16 layouts × 2 trees × 24 benchmarks take about
9 minutes. An A/A run (16 layouts per tree) gave a median interval of ±0.5% across the 33
benchmarks. It flagged 3 of them, against about 1.7 expected by chance at 95%. The
layout-sensitive benchmarks keep wider intervals, ±1–11%, because their layout alone moves
them that far. Comparing one build per side would hide that, not remove it.

To repeat the measurement on other crates, pass one tree with `--seeds 8 --rounds 3` and
look at `rows.jsonl`. `--no-randomize` gives the single-layout control.

### Requirements

Linux, Python 3, and lld 20 or later as the linker. Rust 1.90+ links with its bundled
rust-lld on x86_64 Linux. Older toolchains get `-fuse-ld=lld`, which needs the system's
`ld.lld`.
