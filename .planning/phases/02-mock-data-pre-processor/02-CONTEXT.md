# Phase 2: Mock Data & Pre-processor - Context

**Gathered:** 2026-06-16
**Status:** Ready for planning

<domain>
## Phase Boundary

Create mock benchmark fixture files in the Datadog Benchmarking Platform v1 schema (BP v1) covering baseline and candidate runs, and write a shell script that invokes `bp-analyzer compare pairwise` to produce `artifacts/benchmark-comparison.md`. This markdown comparison is the input to Phase 3 (Claude analysis). No real benchmark runs are needed — the fixtures substitute for what the triggering workstream will eventually supply.

</domain>

<decisions>
## Implementation Decisions

### Input Format
- **D-01:** Fixtures follow the BP v1 schema (`schema_version: v1`, `benchmarks[]` array) — the same format as the `converted.json` files in the provided artifact. Each benchmark entry has `parameters` (name, variant, scenario, git_branch, git_commit_sha, ci_job_date, etc.) and `runs` (`#1`, `#2`, …) with per-metric raw value arrays.
- **D-02:** The corpus consists of multiple files per run (one per benchmark group), not a single monolithic file. Baseline files and candidate files are separate. Example structure: `.gitlab/bench-analysis/fixtures/baseline-<scenario>.json` and `.gitlab/bench-analysis/fixtures/candidate-<scenario>.json`.
- **D-03:** All four metrics are surfaced: `execution_time`, `instructions`, `cpu_user_time`, `max_rss_usage` — each with `uom` and `values` array (~12 raw measurements per run, matching the real artifact structure).

### Pre-processor: bp-analyzer (not jq)
- **D-04:** The pre-processor is `bp-analyzer compare pairwise`, which is pre-installed in `dd-octo-sts-ci-base:2025.06-1`. No install step needed. This replaces the jq script originally described in REQUIREMENTS.md DATA-02.
- **D-05:** Output format: `--format=md --outpath=artifacts/benchmark-comparison.md`. The markdown comparison report is what Phase 3 passes to Claude — not a JSON diff. `benchmark-diff.json` from DATA-02 is superseded by `benchmark-comparison.md`.
- **D-06:** Significance algorithm is fully delegated to `bp-analyzer` (bootstrap confidence intervals at 95% confidence, CI-based `same/unsure/worse/better` verdict per metric). `UNCONFIDENCE_THRESHOLD` defaults to 1%. No custom threshold logic.
- **D-07:** The invocation script should use `--baseline` and `--candidate` JSON selectors matching the `parameters` fields in the fixtures (e.g., `--baseline='{"git_branch":"main"}' --candidate='{"git_branch":"pr-branch"}'`).

### Fixture Content & Coverage
- **D-08:** Fixture scenario names and benchmark names are modeled on the real artifact format but adapted for libdatadog Rust crates. Use actual benchmark names from the codebase (`normalize_service`, `normalize_name`, `span_concentrator`, `obfuscation`) as scenario names, combined with a crate/variant suffix matching the BP schema pattern (e.g., `normalize_service-libdatadog`).
- **D-09:** Coverage requirements per DATA-01: at least one critical regression (~20%+ slower), one noise-level change (within 1% — should produce `same` or `unsure`), one improvement (~15%+ faster), and several unchanged benchmarks. The classification is determined by bp-analyzer from the raw values, not hardcoded in fixtures.
- **D-10:** Mock raw values are constructed so that the statistical signal is unambiguous where intended (regression/improvement: tight distributions with clearly separated means; noise case: overlapping distributions).

### Fixture Location
- **D-11:** Files live in `.gitlab/bench-analysis/fixtures/`. Keeps all bench-analysis CI assets co-located alongside `bench-analysis.yml`.

### Requirements Drift Note
- **D-12** [informational]: DATA-02 in REQUIREMENTS.md describes a "jq script" producing `benchmark-diff.json`. This is superseded by the `bp-analyzer` approach. Planner should note this drift; REQUIREMENTS.md will be updated at phase completion to reflect the actual implementation.

### Claude's Discretion
- Exact number of fixture files and benchmark scenarios (3–6 scenarios is reasonable, covering the DATA-01 classification cases)
- Exact `bp-analyzer` flag set beyond `compare pairwise --format=md --outpath` (e.g., whether to use `--fail_on_regression`)
- Whether a schema validation step (asserting `benchmark-comparison.md` is non-empty) lives in the pre-processor script or in `bench-analysis.yml`

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Benchmark Platform Schema & CLI
- `.gitlab/bench-analysis/fixtures/` — fixture directory (create in this phase; agent should look at sibling `.gitlab/bench-analysis.yml` for structure context)
- `artifacts.zip` extracted at `/tmp/bench-artefacts/.gitlab/benchmarks/artifacts/` — reference artifact showing the real BP v1 format. Key files:
  - `baseline-v26-2.converted.json` — canonical example of the BP v1 schema to model fixtures on
  - `comparison-baseline-vs-candidate-v26-2.md` — example of bp-analyzer markdown output (what `benchmark-comparison.md` will look like)
  - `baseline-v26-2-analysis.md` / `candidate-v26-2-analysis.md` — per-run analysis format

### Existing CI Structure
- `.gitlab/bench-analysis.yml` — the Phase 1 CI job; the pre-processor script integrates here (or is called from here)
- `.gitlab/benchmarks.yml` — structural reference for artifact declaration and script patterns

### Requirements & Roadmap
- `.planning/REQUIREMENTS.md` — DATA-01 and DATA-02 define acceptance criteria; note DATA-02 is superseded (jq → bp-analyzer, benchmark-diff.json → benchmark-comparison.md)
- `.planning/ROADMAP.md` §Phase 2 — success criteria (3 items)

### Real Benchmark Names (for fixture scenario names)
- `libdd-trace-normalization/benches/normalization_utils.rs` — `normalize_service`, `normalize_name` benchmarks
- `libdd-trace-stats/benches/span_concentrator_bench.rs` — `span_concentrator` benchmarks
- `libdd-trace-obfuscation/benches/` — obfuscation benchmarks
- `libdd-sampling/benches/` — sampling benchmarks

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `.gitlab/bench-analysis.yml` — existing CI job script; the pre-processor invocation (`bp-analyzer compare pairwise`) is added as a new script step here, producing `artifacts/benchmark-comparison.md` before Claude is invoked
- `.gitlab/benchmarks.yml` — artifact declaration pattern (`expire_in: 1 month`, `paths: - artifacts/`) already present; Phase 2 ensures `artifacts/benchmark-comparison.md` is in the artifact path

### Established Patterns
- All CI script steps in `.gitlab/bench-analysis.yml` use shell heredocs and explicit `export` — pre-processor script should follow the same style
- `artifacts/` directory is already the declared artifact path; `benchmark-comparison.md` goes there

### Integration Points
- `bench-analysis.yml` script block: add `bp-analyzer compare pairwise` invocation between the existing auth steps and the future Claude invocation (Phase 3)
- Fixture files are committed to the repo under `.gitlab/bench-analysis/fixtures/` and referenced by path in the CI script

</code_context>

<specifics>
## Specific Ideas

- The provided `artifacts.zip` is the ground-truth reference for BP v1 format. Fixture files must match `converted.json` structure exactly so `bp-analyzer` can ingest them without a conversion step.
- Mock raw value arrays should have ~12 values per metric per run (matching the real artifact) so the statistical tests have sufficient sample size.
- `comparison-baseline-vs-candidate-v26-2.md` shows what the comparison output looks like — it's a markdown table with `🟩`/`🟥` emoji classification per metric. This is exactly what Phase 3 Claude will read.

</specifics>

<deferred>
## Deferred Ideas

- Real Criterion-to-BP-v1 converter (a new `bp-analyzer convert` converter for Criterion output) — needed when real benchmark runs land (Augusto's workstream). Out of scope for v1.
- `--fail_on_regression` flag in bp-analyzer invocation to fail the CI job on significant regression — v2 feature; too risky without dedicated benchmark runners.
- Mock dd-trace-py fixtures — blocked on format clarification from Augusto's triggering workstream; v2.

</deferred>

---

*Phase: 2-Mock Data & Pre-processor*
*Context gathered: 2026-06-16*
