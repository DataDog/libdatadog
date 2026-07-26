# Feature Landscape: LLM-Augmented CI Benchmark Analysis Report

**Domain:** CI performance regression analysis with LLM-generated PR comments
**Researched:** 2026-06-15
**Scope:** GitLab CI job producing a GitHub PR comment from Criterion (Rust micro) + dd-trace-py (macro) benchmark results

---

## Table Stakes

Features a reviewer expects to see. Missing any of these and the report is ignored or worse — misleading.

| Feature | Why Expected | Complexity | Notes |
|---------|--------------|------------|-------|
| Overall verdict (pass / warn / fail) | Reviewer needs a single-glance answer before reading details | Low | Keyed off configurable threshold (e.g. >5% regression = warn, >15% = fail) |
| Per-benchmark % change | Primary data point; everything else is commentary | Low | Show `before → after` with ± % for every benchmark in scope |
| Absolute values alongside relative | % alone is misleading (1ns→2ns = +100% but irrelevant) | Low | Show `mean: 42.3 µs → 47.1 µs (+11.3%)` |
| Statistical confidence interval | Criterion emits upper/lower bounds; a change within noise is not a regression | Low | Flag changes that are within CI bounds as "within noise" |
| Separate regression / improvement / unchanged sections | Cognitive load: reviewers scan for regressions first | Low | Three sections; unchanged benchmarks collapsed by default |
| Source identification (Criterion vs dd-trace-py) | Two different suites; a macro regression matters differently than a micro one | Low | Label each benchmark with suite name |
| Link to raw artifact | Reviewers need to be able to audit the raw data | Low | CI artifact URL in comment footer |

## Differentiators

High-value additions that go beyond raw numbers. These are where the LLM earns its place.

| Feature | Value Proposition | Complexity | Notes |
|---------|-------------------|------------|-------|
| Natural-language summary of what regressed | Translates data into a sentence a contributor can act on ("median allocation in `encode_span` grew 18%; likely from the new Vec pre-allocation in `#[commit abc]`") | Medium | LLM's primary job; requires git diff context as input |
| Suspect code change pointer | Correlates the benchmark name with the files/functions changed in the PR diff; narrows "what to look at" to a few lines | Medium | Pass `git diff --stat` + relevant file diffs to the LLM prompt; LLM flags the overlap |
| Severity classification | Distinguish noise (< threshold), notable (threshold–2×), and critical (>2×) regressions | Low | Drives the overall verdict color; avoids false alarms on µs-level changes in ms-range benchmarks |
| Noise warning | CI runners are inherently noisy; a result without a noise caveat trains reviewers to ignore alerts | Low | If benchmark CI is `>= 5%` of estimated value, flag result as "high variance — interpret with caution" |
| Improvement callout | Teams invest in perf work; surfacing wins alongside regressions creates positive reinforcement | Low | Often skipped; easy to add and appreciated |
| Grouped by logical area | Criterion benchmark IDs often encode module/function hierarchy; grouping by prefix reduces scan time | Low | Parse benchmark ID on `/` separator |

## Anti-Features

Things to deliberately exclude. Including them degrades the report's utility.

| Anti-Feature | Why Avoid | What to Do Instead |
|--------------|-----------|-------------------|
| Every benchmark result in the main comment body | A table of 200 micro-benchmarks is skimmed once and then ignored forever; trains reviewers to rubber-stamp | Inline only regressions and improvements; put full table in a `<details>` fold or artifact link |
| Flame graph in the PR comment | Flame graphs are SVG/HTML; they don't render in GitHub comments and linking to them adds noise when there's no regression | Only mention flame graph artifact if a critical regression is detected, as a "next step" pointer |
| Trend over time in the PR comment | Historical graphs require external storage (GitHub Pages, S3); adds infra complexity for marginal PR-comment value | Defer to a follow-up "continuous benchmarking from main" workstream; link to Bencher/CodSpeed if adopted later |
| Exact sample distributions / histograms | Full Criterion sample data is hundreds of rows; the LLM summary replaces this | Use mean ± stddev; Criterion confidence intervals cover the statistical need |
| Automated PR approval/rejection via GitHub status check | A benchmark in a noisy shared CI runner failing the PR blocks merges on false positives | Post as informational comment; let the engineer decide; consider a required check only after migrating to dedicated benchmark runners |
| Raw iteration counts | Internal Criterion detail; not actionable for reviewers | Strip from display; keep in artifact |
| LLM confidence scores or "I think" hedging | Adds verbal noise; reviewers don't care about LLM uncertainty, they care about the data | LLM should state findings directly; caveat only when data is genuinely ambiguous (high-variance measurement) |
| Repeated boilerplate preamble on every comment update | If the job re-runs, an updated comment is better than a new comment with the same preamble | Find-and-replace the existing comment via GitHub API `PATCH /repos/{owner}/{repo}/issues/comments/{id}` |

---

## Feature Dependencies

```
Overall verdict → regression detection with threshold
Regression detection with threshold → per-benchmark % change + absolute values
Suspect code change pointer → git diff of PR branch fed to LLM
Noise warning → Criterion confidence interval bounds in input data
Grouped by logical area → benchmark ID parsing
```

---

## MVP Recommendation

Prioritize for the first shipped version:

1. Overall verdict (pass / warn / fail) with configurable threshold
2. Per-benchmark % change with absolute values and confidence-interval noise guard
3. Three sections: regressions / improvements / unchanged (last section collapsed)
4. LLM-generated natural-language summary paragraph per regression
5. Suspect code change pointer (pass PR diff to LLM; ask it to name overlapping files/functions)
6. Improvement callout (same effort as regression, builds goodwill)
7. Suite labeling (Criterion vs dd-trace-py)
8. Raw artifact link in footer

Defer to follow-up:

- **Trend over time**: requires persistent storage outside this job; separate workstream
- **Flame graph integration**: requires CodSpeed or a profiling pass; not available in scope
- **Dedicated benchmark runner**: eliminates noise problem but is an infra decision beyond this job
- **Required PR status check**: unsafe until noise is controlled; ship as informational first

---

## Mock Data Shapes Required for End-to-End Testing

To test the pipeline without real benchmark runs, two fixture files are needed.

### Criterion mock (Rust micro) — `criterion_results.json`

NDJSON (one object per line), `cargo-criterion --message-format=json` format. Each record needs:

```json
{
  "reason": "benchmark-complete",
  "id": "encode_span/small_span",
  "typical": { "estimate": 42300.0, "lower_bound": 41900.0, "upper_bound": 42700.0, "unit": "ns" },
  "mean":    { "estimate": 42450.0, "lower_bound": 41800.0, "upper_bound": 43100.0, "unit": "ns" },
  "median":  { "estimate": 42200.0, "lower_bound": 41700.0, "upper_bound": 42600.0, "unit": "ns" },
  "change": {
    "mean":   { "estimate": 0.113, "lower_bound": 0.091, "upper_bound": 0.136 },
    "median": { "estimate": 0.108, "lower_bound": 0.088, "upper_bound": 0.129 }
  }
}
```

The fixture set must include: at least one critical regression (>15%), one minor regression (5–15%), one improvement, and several unchanged benchmarks — spread across at least two benchmark group prefixes.

### dd-trace-py mock (Python macro) — `ddtrace_results.json`

pytest-benchmark JSON format (`pytest --benchmark-json`). Top-level structure:

```json
{
  "machine_info": { "python_implementation": "CPython", "python_version": "3.11.0" },
  "commit_info":  { "id": "<sha>", "branch": "main" },
  "benchmarks": [
    {
      "name": "test_trace_encoding[small_trace]",
      "stats": {
        "mean": 0.000423,
        "stddev": 0.0000085,
        "median": 0.000420,
        "min": 0.000415,
        "max": 0.000438,
        "ops": 2364.1
      }
    }
  ]
}
```

The fixture set must pair a "before" (baseline/main) file and an "after" (PR branch) file for each suite, so the pipeline can compute deltas.

---

## Sources

- [Bencher - Continuous Benchmarking](https://bencher.dev/)
- [CodSpeed: Benchmarks in CI without noise](https://codspeed.io/blog/benchmarks-in-ci-without-noise)
- [criterion-compare-action (boa-dev)](https://github.com/boa-dev/criterion-compare-action)
- [github-action-benchmark](https://github.com/benchmark-action/github-action-benchmark)
- [critcmp](https://github.com/BurntSushi/critcmp)
- [cargo-criterion external tools / JSON format](https://bheisler.github.io/criterion.rs/book/cargo_criterion/external_tools.html)
- [pytest-benchmark usage docs](https://pytest-benchmark.readthedocs.io/en/latest/usage.html)
- [ddtrace benchmarks docs](https://ddtrace.readthedocs.io/en/stable/benchmarks.html)
- [Detecting Tiny Performance Regressions at Hyperscale (FBDetect, ACM)](https://dl.acm.org/doi/pdf/10.1145/3785504)
- [GitHub collapsible sections docs](https://docs.github.com/en/get-started/writing-on-github/working-with-advanced-formatting/organizing-information-with-collapsed-sections)
- [CodSpeed prior art / Bencher comparison](https://bencher.dev/docs/reference/prior-art/)
