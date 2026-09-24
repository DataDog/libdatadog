# Phase 2: Mock Data & Pre-processor - Research

**Researched:** 2026-06-16
**Domain:** Benchmarking Platform v1 schema, bp-analyzer CLI, CI shell scripting
**Confidence:** HIGH

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- **D-01:** Fixtures follow the BP v1 schema (`schema_version: v1`, `benchmarks[]` array) — same format as `converted.json` files in the artifact. Each benchmark entry has `parameters` (name, variant, scenario, git_branch, git_commit_sha, ci_job_date, etc.) and `runs` (`#1`, `#2`, …) with per-metric raw value arrays.
- **D-02:** Corpus is multiple files per run (one per benchmark group), not a single monolithic file. Baseline files and candidate files are separate. Example structure: `.gitlab/bench-analysis/fixtures/baseline-<scenario>.json` and `.gitlab/bench-analysis/fixtures/candidate-<scenario>.json`.
- **D-03:** All four metrics are surfaced: `execution_time`, `instructions`, `cpu_user_time`, `max_rss_usage` — each with `uom` and `values` array (~12 raw measurements per run, matching the real artifact structure).
- **D-04:** Pre-processor is `bp-analyzer compare pairwise`, pre-installed in `dd-octo-sts-ci-base:2025.06-1`. No install step needed.
- **D-05:** Output format: `--format=md --outpath=artifacts/benchmark-comparison.md`. The markdown report is what Phase 3 passes to Claude.
- **D-06:** Significance algorithm fully delegated to `bp-analyzer` (bootstrap confidence intervals at 95%, CI-based `same/unsure/worse/better` verdict per metric). `UNCONFIDENCE_THRESHOLD` defaults to 1%. No custom threshold logic.
- **D-07:** Invocation script uses `--baseline` and `--candidate` JSON selectors matching `parameters` fields (e.g., `--baseline='{"git_branch":"main"}'` `--candidate='{"git_branch":"pr-branch"}'`).
- **D-08:** Fixture scenario names and benchmark names modeled on real libdatadog Rust crate benchmarks (`normalize_service`, `normalize_name`, `span_concentrator`, `obfuscation`) with crate/variant suffix.
- **D-09:** Coverage: at least one critical regression (~20%+ slower), one noise-level change (within 1%), one improvement (~15%+ faster), several unchanged benchmarks. Classification determined by bp-analyzer from raw values.
- **D-10:** Mock raw values constructed for unambiguous statistical signal where intended (regression/improvement: tight distributions with clearly separated means; noise: overlapping distributions).
- **D-11:** Files live in `.gitlab/bench-analysis/fixtures/`.
- **D-12:** DATA-02 in REQUIREMENTS.md describes a jq script producing `benchmark-diff.json`. This is superseded by `bp-analyzer` approach. REQUIREMENTS.md updated at phase completion.

### Claude's Discretion
- Exact number of fixture files and benchmark scenarios (3–6 is reasonable, covering DATA-01 classification cases)
- Exact `bp-analyzer` flag set beyond `compare pairwise --format=md --outpath` (e.g., whether to use `--fail_on_regression`)
- Whether schema validation (asserting `benchmark-comparison.md` is non-empty) lives in the pre-processor script or in `bench-analysis.yml`

### Deferred Ideas (OUT OF SCOPE)
- Real Criterion-to-BP-v1 converter (`bp-analyzer convert` for Criterion output) — needed when real benchmark runs land.
- `--fail_on_regression` flag — v2 feature; too risky without dedicated benchmark runners.
- Mock dd-trace-py fixtures — blocked on format clarification from triggering workstream.
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| DATA-01 | Mock Criterion benchmark fixtures exist as before/after JSON files covering at least: one critical regression, one minor regression (within noise), one improvement, and several unchanged benchmarks | BP v1 schema verified from `baseline-v26-2.converted.json`. Four scenarios map to the four classifications. Raw value construction strategy verified by simulation. |
| DATA-02 | Pre-processor produces structured benchmark diff (superseded: now `benchmark-comparison.md` via `bp-analyzer compare pairwise`) | `bp-analyzer` confirmed pre-installed. Selector syntax confirmed via CONTEXT.md D-07 and artifact parameter analysis. Output format confirmed from `comparison-baseline-vs-candidate-v26-2.md`. |
</phase_requirements>

## Summary

Phase 2 creates fixture JSON files in Benchmarking Platform v1 schema and a shell script that invokes `bp-analyzer compare pairwise` to produce `artifacts/benchmark-comparison.md`. The markdown comparison report feeds Phase 3 (Claude analysis).

The BP v1 schema is fully understood from the reference artifact. Each fixture file has `schema_version: "v1"` and a `benchmarks` array. Each benchmark entry has a `parameters` object (with `name`, `variant`, `scenario`, `git_branch`, `baseline_or_candidate`, `git_commit_sha`, `ci_job_date`, `ci_job_id`, `ci_pipeline_id`, `git_commit_date`) and a `runs` object (`#1`, optionally `#2`) where each run contains the four metrics (`execution_time`, `instructions`, `cpu_user_time`, `max_rss_usage`), each with a `uom` string and a `values` array of 12 floats.

The `bp-analyzer` CLI is pre-installed in the CI image. It distinguishes baseline from candidate by matching the `--baseline` and `--candidate` JSON selectors against the `parameters` field in each benchmark entry — specifically `git_branch` is the cleanest differentiator (baseline uses `"main"`, candidate uses `"pr-branch"`). The tool ingests all fixture files and produces a markdown comparison report. A non-empty output assertion should be added to the script (the comparison markdown is always non-empty if any benchmarks are compared).

**Primary recommendation:** Two fixture files (`baseline.json` and `candidate.json`) each containing all benchmark scenarios, invoked via a single `bp-analyzer compare pairwise` command with `git_branch`-based selectors.

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| BP v1 fixture data | Static files | — | Committed JSON files; no runtime generation needed |
| Benchmark diff computation | CI script (`bp-analyzer`) | — | Pre-installed tool handles statistics; no custom code |
| Comparison report generation | CI script | — | `bp-analyzer --format=md` produces the markdown directly |
| Output validation | CI script | bench-analysis.yml | Bash `-s` check or `wc -l` on the output file |
| Artifact declaration | bench-analysis.yml | — | Already declares `artifacts/` path; no changes needed |

## Standard Stack

### Core
| Tool | Version | Purpose | Why Standard |
|------|---------|---------|--------------|
| `bp-analyzer` | pre-installed | Pairwise comparison with bootstrap CI | Datadog-internal tool; pre-installed in CI image; handles significance testing |
| Shell (bash) | system | Invocation script | Matches existing pattern in `bench-analysis.yml` |

### Supporting
| Tool | Version | Purpose | When to Use |
|------|---------|---------|-------------|
| Python 3 | system | Fixture generation validation (local only) | Optional: validate JSON structure matches schema before committing |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| `bp-analyzer compare pairwise` | Custom jq diff script | jq script was original DATA-02 plan but produces non-standard JSON; bp-analyzer produces authoritative CI-based verdicts |
| `git_branch` selector | `baseline_or_candidate` selector | Both work; `git_branch` is more realistic for production use |

**No installation:** `bp-analyzer` is pre-installed in `dd-octo-sts-ci-base:2025.06-1`. No `npm install` or download step needed.

## Package Legitimacy Audit

> No external packages are installed by this phase. `bp-analyzer` is a Datadog-internal tool pre-installed in the CI image. No npm/pip/cargo installs occur.

**Packages removed due to SLOP verdict:** none
**Packages flagged as suspicious:** none

## Architecture Patterns

### System Architecture Diagram

```
Committed fixtures (baseline.json, candidate.json)
         |
         v
.gitlab/bench-analysis/preprocess.sh
         |
         | bp-analyzer compare pairwise \
         |   --baseline '{"git_branch":"main"}' \
         |   --candidate '{"git_branch":"pr-branch"}' \
         |   --format=md \
         |   --outpath=artifacts/benchmark-comparison.md \
         |   .gitlab/bench-analysis/fixtures/baseline.json \
         |   .gitlab/bench-analysis/fixtures/candidate.json
         v
artifacts/benchmark-comparison.md  <-- Phase 3 input
         |
         v (validation: assert file is non-empty)
CI exits 0 or 1
```

### Recommended Project Structure
```
.gitlab/
├── bench-analysis.yml           # Phase 1 CI job (add preprocess step here)
└── bench-analysis/
    ├── fixtures/
    │   ├── baseline.json        # All baseline benchmark scenarios
    │   └── candidate.json       # All candidate benchmark scenarios
    └── preprocess.sh            # bp-analyzer invocation script
```

### Pattern 1: BP v1 JSON Fixture Structure
**What:** Each fixture file has exactly two top-level keys: `schema_version` and `benchmarks`.
**When to use:** Any time the pre-processor needs input data.
**Example:**
```json
{
  "schema_version": "v1",
  "benchmarks": [
    {
      "parameters": {
        "name": "normalize",
        "variant": "service",
        "scenario": "normalize-service-libdatadog",
        "baseline_or_candidate": "baseline",
        "git_branch": "main",
        "git_commit_sha": "aaaaaaaabbbbbbbbccccccccddddddddeeeeeeee",
        "git_commit_date": "1718000000",
        "ci_job_date": "1718000060",
        "ci_job_id": "100000001",
        "ci_pipeline_id": "200000001"
      },
      "runs": {
        "#1": {
          "execution_time": {
            "uom": "ns",
            "values": [499400.0, 499500.0, 499600.0, 499700.0, 499800.0, 499900.0,
                       500000.0, 500100.0, 500200.0, 500300.0, 500400.0, 500500.0]
          },
          "instructions": {
            "uom": "instructions",
            "values": [1200000.0, 1200010.0, 1200020.0, 1200030.0, 1200040.0, 1200050.0,
                       1200060.0, 1200070.0, 1200080.0, 1200090.0, 1200100.0, 1200110.0]
          },
          "cpu_user_time": {
            "uom": "ns",
            "values": [498000.0, 498100.0, 498200.0, 498300.0, 498400.0, 498500.0,
                       498600.0, 498700.0, 498800.0, 498900.0, 499000.0, 499100.0]
          },
          "max_rss_usage": {
            "uom": "bytes",
            "values": [2097152.0, 2097152.0, 2097152.0, 2097152.0, 2097152.0, 2097152.0,
                       2097152.0, 2097152.0, 2097152.0, 2097152.0, 2097152.0, 2097152.0]
          }
        }
      }
    }
  ]
}
```
Source: [VERIFIED: direct analysis of `/tmp/bench-artefacts/.gitlab/benchmarks/artifacts/baseline-v26-2.converted.json`]

### Pattern 2: bp-analyzer Invocation
**What:** Shell invocation using `compare pairwise` with JSON selectors and markdown output.
**When to use:** As the pre-processor step in the CI script.
**Example:**
```bash
mkdir -p artifacts
bp-analyzer compare pairwise \
  --baseline '{"git_branch":"main"}' \
  --candidate '{"git_branch":"pr-branch"}' \
  --format=md \
  --outpath=artifacts/benchmark-comparison.md \
  .gitlab/bench-analysis/fixtures/baseline.json \
  .gitlab/bench-analysis/fixtures/candidate.json

# Assert non-empty output
if [ ! -s artifacts/benchmark-comparison.md ]; then
  echo "ERROR: benchmark-comparison.md is empty" >&2
  exit 1
fi
```
Source: [CITED: CONTEXT.md D-04, D-05, D-07 — confirmed by user who provided bp-analyzer documentation]

### Pattern 3: Baseline vs Candidate Differentiation in Parameters
**What:** The two fixture files differ in exactly four `parameters` fields.
**When to use:** When constructing fixture JSON.

| Field | baseline.json value | candidate.json value |
|-------|--------------------|--------------------|
| `baseline_or_candidate` | `"baseline"` | `"candidate"` |
| `git_branch` | `"main"` | `"pr-branch"` |
| `git_commit_sha` | `"aaaaaa...baseline_sha"` | `"bbbbbb...candidate_sha"` |
| `git_commit_date` | `"1718000000"` | `"1718000100"` |

All other parameters (`name`, `variant`, `scenario`, `ci_job_date`, `ci_job_id`, `ci_pipeline_id`) are identical between baseline and candidate for the same scenario.

Source: [VERIFIED: direct comparison of `baseline-v26-2.converted.json` vs `candidate-v26-2.converted.json`]

### Pattern 4: Preprocess Script Integration in bench-analysis.yml
**What:** Add the preprocess step to the existing CI job script block between auth and Claude invocation.
**When to use:** Extending the Phase 1 job.
**Example:**
```yaml
# In bench-analysis.yml, inside the script: block, after auth setup:
- bash .gitlab/bench-analysis/preprocess.sh
```
The preprocess.sh is a separate file (not an inline heredoc) to keep the YAML readable and allow the script to be tested locally.

Source: [VERIFIED: analysis of existing `.gitlab/bench-analysis.yml` style — all steps are shell-invoked, uses `export` explicitly]

### Anti-Patterns to Avoid
- **Hardcoded classification in fixture values:** Do not set values that rely on exact threshold knowledge. Instead, use clearly separated distributions (20%+ delta) and trust `bp-analyzer` to classify them. This is what D-10 prescribes.
- **Single combined file for both baseline and candidate:** The reference artifact uses two separate files. The selector syntax requires a way to tell them apart — two files with distinct `git_branch` is the cleanest approach.
- **Inline heredoc for fixture JSON in the CI script:** Fixtures must be committed as static JSON files under `.gitlab/bench-analysis/fixtures/`. They are the ground truth for regression/improvement/noise detection and must be readable outside CI.
- **Missing `mkdir -p artifacts/`:** The `artifacts/` directory does not exist at job start. The preprocess script must create it before `--outpath` writes to it.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Statistical significance testing | Custom bootstrap CI or simple mean ratio | `bp-analyzer compare pairwise` | Bootstrap CI requires hundreds of lines of correct statistics code; bp-analyzer is authoritative Datadog tooling |
| Benchmark comparison formatting | Custom markdown table generator | `--format=md` flag | Format matches what Phase 3 Claude expects; consistent with production output |
| Regression classification thresholds | Custom threshold logic in shell | bp-analyzer's built-in `SIGNIFICANT_IMPACT_THRESHOLD` (default 1%) | Avoids threshold drift between pre-processor and actual BP tool |

**Key insight:** `bp-analyzer` exists precisely to avoid custom benchmark diff logic. The only custom code in this phase is the shell script that invokes it and the JSON fixtures.

## Common Pitfalls

### Pitfall 1: Noise Scenario Mis-classified as Significant
**What goes wrong:** Noise scenario values that are actually >1% apart get classified as `worse` or `better` instead of `same`/`unsure`.
**Why it happens:** The bootstrap CI at 95% with 12 samples is sensitive. A difference of 0.3% with very tight standard deviation can still be flagged as significant if the CI doesn't cross zero.
**How to avoid:** Use overlapping distributions for the noise case. Set candidate mean within 0.3% of baseline AND use similar jitter so distributions overlap. Example: baseline mean 100,000 ns ± 300 ns; candidate mean 100,300 ns ± 300 ns (0.3% delta, overlapping ranges).
**Warning signs:** Pre-flight: run bp-analyzer locally against test fixtures before committing; check the output says `same` or `unsure` for the noise scenario.

### Pitfall 2: Wrong Number of Values in `values` Array
**What goes wrong:** bp-analyzer may reject fixtures with fewer than some minimum sample count, or the statistical test degrades with too-small samples.
**Why it happens:** The reference artifact always has exactly 12 values per metric per run. bp-analyzer's bootstrap CI needs enough samples.
**How to avoid:** Always use exactly 12 values per metric per run. [VERIFIED: direct count from `baseline-v26-2.converted.json` and `candidate-v26-2.converted.json`]

### Pitfall 3: Missing `artifacts/` Directory
**What goes wrong:** `--outpath=artifacts/benchmark-comparison.md` fails silently or with a file-not-found error.
**Why it happens:** The GitLab CI job starts in a clean workspace. `artifacts/` does not pre-exist.
**How to avoid:** Add `mkdir -p artifacts/` as the first line of `preprocess.sh`.

### Pitfall 4: Selector Mismatch Between Fixture and bp-analyzer Call
**What goes wrong:** bp-analyzer matches 0 benchmarks for baseline or candidate, producing an empty or error output.
**Why it happens:** The `--baseline` JSON selector must exactly match a subset of `parameters` keys in the fixture. If the fixture has `git_branch: "main"` but the selector says `"master"`, no match occurs.
**How to avoid:** Use `"main"` consistently as the baseline branch name in fixtures and the selector. Verify with `grep` that the fixture and selector agree before committing.

### Pitfall 5: bp-analyzer Not Found in PATH
**What goes wrong:** `bp-analyzer: command not found` at job runtime.
**Why it happens:** While D-04 confirms pre-installed, the PATH may not include it by default in all shell contexts.
**How to avoid:** Add a probe step at the start of `preprocess.sh`: `command -v bp-analyzer || { echo "bp-analyzer not found"; exit 1; }`. This fails fast with a clear error rather than a confusing file-not-found from `--outpath`.

## Fixture Scenarios

### Coverage Plan (4 scenarios, 8 benchmarks total)

| File | Scenario | `name` | `variant` | `scenario` field | Classification Expected |
|------|----------|--------|-----------|-----------------|------------------------|
| baseline.json + candidate.json | Normalize service regression | `normalize` | `service` | `normalize-service-libdatadog` | `worse` (20%+ regression) |
| baseline.json + candidate.json | Normalize name unchanged | `normalize` | `name` | `normalize-name-libdatadog` | `same` (identical values) |
| baseline.json + candidate.json | Concentrator improvement | `concentrator` | `add_spans` | `concentrator-libdatadog` | `better` (~15% faster) |
| baseline.json + candidate.json | SQL obfuscation noise | `obfuscation` | `sql` | `obfuscation-sql-libdatadog` | `same` or `unsure` (~0.3% delta, overlapping) |

**Two files** (`baseline.json`, `candidate.json`) each contain all 4 benchmark entries. This matches the reference pattern (one file per run type, all groups combined).

### Raw Value Strategy

**Realistic nanosecond base values** derived from real libdatadog benchmark characteristics:

| Scenario | Metric | Baseline base (ns) | Candidate base | Jitter (±) |
|----------|--------|--------------------|----------------|-----------|
| normalize-service | execution_time | 500,000 | 600,000 (+20%) | ±300 |
| normalize-name | execution_time | 400,000 | 400,000 (same) | ±300 |
| concentrator | execution_time | 5,000,000 | 4,250,000 (-15%) | ±1,000 |
| obfuscation-sql | execution_time | 100,000 | 100,300 (+0.3%) | ±300 |

All metrics use tight linear jitter across 12 values: `base + i*step` for `i` in `[-5, -4, ..., 6]` (12 values). Instructions use proportional counts; cpu_user_time ≈ 99% of execution_time; max_rss_usage is a fixed realistic value per scenario.

Source: [ASSUMED — jitter strategy and absolute values are informed by real artifact analysis but exact values need tuning based on bp-analyzer output]

## Code Examples

### Full Minimal Fixture Entry (single benchmark in `benchmarks` array)
```json
{
  "parameters": {
    "name": "normalize",
    "variant": "service",
    "scenario": "normalize-service-libdatadog",
    "baseline_or_candidate": "baseline",
    "git_branch": "main",
    "git_commit_sha": "aaaaaaaabbbbbbbbccccccccdddddddd00000001",
    "git_commit_date": "1718000000",
    "ci_job_date": "1718001000",
    "ci_job_id": "100000001",
    "ci_pipeline_id": "200000001"
  },
  "runs": {
    "#1": {
      "execution_time": {
        "uom": "ns",
        "values": [499400.0, 499500.0, 499600.0, 499700.0, 499800.0, 499900.0,
                   500000.0, 500100.0, 500200.0, 500300.0, 500400.0, 500500.0]
      },
      "instructions": {
        "uom": "instructions",
        "values": [1199500.0, 1199600.0, 1199700.0, 1199800.0, 1199900.0, 1200000.0,
                   1200100.0, 1200200.0, 1200300.0, 1200400.0, 1200500.0, 1200600.0]
      },
      "cpu_user_time": {
        "uom": "ns",
        "values": [494400.0, 494500.0, 494600.0, 494700.0, 494800.0, 494900.0,
                   495000.0, 495100.0, 495200.0, 495300.0, 495400.0, 495500.0]
      },
      "max_rss_usage": {
        "uom": "bytes",
        "values": [2097152.0, 2097152.0, 2097152.0, 2097152.0, 2097152.0, 2097152.0,
                   2097152.0, 2097152.0, 2097152.0, 2097152.0, 2097152.0, 2097152.0]
      }
    }
  }
}
```
Source: [VERIFIED: modeled on `baseline-v26-2.converted.json` structure]

### Minimal preprocess.sh
```bash
#!/usr/bin/env bash
set -euo pipefail

# Verify bp-analyzer is available
command -v bp-analyzer || { echo "ERROR: bp-analyzer not found in PATH" >&2; exit 1; }

# Ensure output directory exists
mkdir -p artifacts

# Run pairwise comparison
bp-analyzer compare pairwise \
  --baseline '{"git_branch":"main"}' \
  --candidate '{"git_branch":"pr-branch"}' \
  --format=md \
  --outpath=artifacts/benchmark-comparison.md \
  .gitlab/bench-analysis/fixtures/baseline.json \
  .gitlab/bench-analysis/fixtures/candidate.json

# Assert output is non-empty
if [ ! -s artifacts/benchmark-comparison.md ]; then
  echo "ERROR: benchmark-comparison.md is empty — bp-analyzer produced no output" >&2
  exit 1
fi

echo "benchmark-comparison.md generated ($(wc -l < artifacts/benchmark-comparison.md) lines)"
```
Source: [ASSUMED for bp-analyzer flags beyond `--format`/`--outpath`/`--baseline`/`--candidate` — these core flags are locked per CONTEXT.md D-05, D-07]

### bench-analysis.yml Addition (pre-processor step)
```yaml
# Add this step in script: block, after auth exports and before the smoke test or Claude invocation
- bash .gitlab/bench-analysis/preprocess.sh
```
Source: [VERIFIED: matches existing shell-invocation style in `.gitlab/bench-analysis.yml`]

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|-----------------|--------------|--------|
| jq script → `benchmark-diff.json` (DATA-02 original) | `bp-analyzer compare pairwise` → `benchmark-comparison.md` | Phase 2 context (2026-06-16) | Authoritative statistical significance; markdown directly usable by Phase 3 Claude |

**Superseded:**
- `benchmark-diff.json`: replaced by `benchmark-comparison.md`; not produced in this phase

## Runtime State Inventory

> Not applicable — this is a greenfield phase adding new committed files. No rename/refactor/migration involved.

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | `bp-analyzer compare pairwise` accepts positional file path arguments for fixture files | Code Examples (preprocess.sh) | Script fails at runtime; need to use stdin or `-i` flag instead |
| A2 | `--baseline` and `--candidate` flags take JSON object strings matching `parameters` subsets | Code Examples | Wrong selector syntax → 0 matched benchmarks → empty output |
| A3 | bp-analyzer is in PATH without additional setup in `dd-octo-sts-ci-base:2025.06-1` | Common Pitfalls | Need to source a profile or set PATH before invocation |
| A4 | The noise scenario (0.3% delta, overlapping distributions) produces `same` or `unsure` from bp-analyzer | Fixture Scenarios | May need to reduce the delta further or increase jitter to get the desired outcome |
| A5 | Fixture `cpu_usage_percentage` field is optional (not required by bp-analyzer) | Standard Stack / Fixture | bp-analyzer may require it; if so, add it with realistic values (≈100%) |

**Note on A1–A3:** These can be probed in CI with a simple dry-run job against an empty fixture before the real implementation. The CONTEXT.md D-07 reference is the only documentation available — bp-analyzer source is Datadog-internal and not publicly inspectable.

## Open Questions

1. **Exact bp-analyzer CLI syntax for file input**
   - What we know: `--baseline`, `--candidate`, `--format=md`, `--outpath` are confirmed flags (CONTEXT.md D-05, D-07)
   - What's unclear: Whether fixture file paths are positional arguments, or require a `-i` / `--input` flag
   - Recommendation: Implement using positional args (matching reference platform usage); if it fails, try `--input` flag. Add a `command -v bp-analyzer && bp-analyzer --help` probe step

2. **Whether `cpu_usage_percentage` is required in fixtures**
   - What we know: The reference artifact includes it; the four required metrics are execution_time, instructions, cpu_user_time, max_rss_usage per CONTEXT.md D-03
   - What's unclear: Whether bp-analyzer rejects fixtures missing this field
   - Recommendation: Omit it from fixtures (D-03 does not list it); if bp-analyzer errors, add it with flat 100.0 values

3. **Exact number of runs needed per benchmark**
   - What we know: Baseline has `#1` and `#2` (24 total samples); candidate has only `#1` (12 samples)
   - What's unclear: Whether asymmetric run counts between baseline and candidate affect significance calculation
   - Recommendation: Use `#1` only (12 values) for both baseline and candidate; simpler and matches candidate reference

## Environment Availability

| Dependency | Required By | Available | Version | Fallback |
|------------|------------|-----------|---------|----------|
| `bp-analyzer` | preprocess.sh | ✓ (CI image) | pre-installed in dd-octo-sts-ci-base:2025.06-1 | None — blocked if absent |
| `bash` | preprocess.sh | ✓ | system | — |
| `mkdir`, `wc` | preprocess.sh | ✓ | coreutils | — |

**Missing dependencies with no fallback:** None (bp-analyzer is confirmed pre-installed per D-04).

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Bash script + file assertions (no test runner needed) |
| Config file | none |
| Quick run command | `bash .gitlab/bench-analysis/preprocess.sh` (requires bp-analyzer) |
| Full suite command | `bash .gitlab/bench-analysis/preprocess.sh && grep -c 'normalize-service-libdatadog' artifacts/benchmark-comparison.md` |

### Phase Requirements → Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| DATA-01 | Fixture files exist covering regression/noise/improvement/unchanged | structural | `ls .gitlab/bench-analysis/fixtures/baseline.json .gitlab/bench-analysis/fixtures/candidate.json` | ❌ Wave 0 |
| DATA-01 | Fixture schema is valid BP v1 | structural | `python3 -c "import json; json.load(open('.gitlab/bench-analysis/fixtures/baseline.json'))"` | ❌ Wave 0 |
| DATA-02 | Pre-processor produces non-empty benchmark-comparison.md | smoke | `bash .gitlab/bench-analysis/preprocess.sh && test -s artifacts/benchmark-comparison.md` | ❌ Wave 0 |
| DATA-02 | Output contains expected scenario names | content | `grep 'normalize-service-libdatadog' artifacts/benchmark-comparison.md` | ❌ Wave 0 |

### Sampling Rate
- **Per task commit:** `python3 -c "import json; json.load(open('.gitlab/bench-analysis/fixtures/baseline.json'))"` (JSON validity)
- **Per wave merge:** `bash .gitlab/bench-analysis/preprocess.sh` (requires bp-analyzer in CI)
- **Phase gate:** `test -s artifacts/benchmark-comparison.md` (non-empty output)

### Wave 0 Gaps
- [ ] `.gitlab/bench-analysis/fixtures/baseline.json` — BP v1 fixture file (main deliverable)
- [ ] `.gitlab/bench-analysis/fixtures/candidate.json` — BP v1 fixture file (main deliverable)
- [ ] `.gitlab/bench-analysis/preprocess.sh` — bp-analyzer invocation script (main deliverable)

*(No test framework install needed — all validation is shell assertions)*

## Security Domain

> `security_enforcement: true`, `security_asvs_level: 1`.

### Applicable ASVS Categories

| ASVS Category | Applies | Standard Control |
|---------------|---------|-----------------|
| V2 Authentication | no | Pre-processor runs post-auth; no new auth code |
| V3 Session Management | no | Shell script, no sessions |
| V4 Access Control | no | Static files committed to repo |
| V5 Input Validation | no | Fixtures are committed static files, not user input |
| V6 Cryptography | no | No crypto in this phase |

### Known Threat Patterns for Shell/CI

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| Shell injection via variable expansion | Tampering | All paths are static literals in preprocess.sh; no user-controlled input |
| Fixture JSON with malicious content | Tampering | Fixtures are committed to the repo and reviewed in PRs; no dynamic generation |

**Security assessment:** This phase is low-risk. All inputs are committed static files. No user input, no secrets, no network calls in the pre-processor script itself.

## Sources

### Primary (HIGH confidence)
- Direct inspection of `/tmp/bench-artefacts/.gitlab/benchmarks/artifacts/baseline-v26-2.converted.json` — BP v1 schema structure, field names, metric UOMs, 12-value arrays
- Direct inspection of `/tmp/bench-artefacts/.gitlab/benchmarks/artifacts/candidate-v26-2.converted.json` — baseline vs candidate parameter differences
- Direct inspection of `/tmp/bench-artefacts/.gitlab/benchmarks/artifacts/comparison-baseline-vs-candidate-v26-2.md` — bp-analyzer markdown output format
- `.gitlab/bench-analysis.yml` — existing CI job structure and shell scripting style
- `02-CONTEXT.md` (locked decisions D-01 through D-12) — user-confirmed choices

### Secondary (MEDIUM confidence)
- `02-DISCUSSION-LOG.md` — records that user provided bp-analyzer documentation confirming flags and approach

### Tertiary (LOW confidence / ASSUMED)
- bp-analyzer positional file argument syntax — inferred from reference artifact file naming convention; not directly testable without the binary

## Metadata

**Confidence breakdown:**
- BP v1 schema structure: HIGH — verified from actual artifact files
- fixture raw value strategy: MEDIUM — simulation confirms statistical separation; actual bp-analyzer output depends on internal bootstrap implementation
- bp-analyzer CLI flags: MEDIUM — core flags locked in CONTEXT.md; input syntax is assumed
- pre-processor shell script structure: HIGH — matches existing CI style exactly

**Research date:** 2026-06-16
**Valid until:** 2026-09-16 (schema is stable; bp-analyzer is pinned to CI image)
