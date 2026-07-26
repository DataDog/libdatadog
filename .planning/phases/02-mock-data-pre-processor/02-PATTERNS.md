# Phase 2: Mock Data & Pre-processor - Pattern Map

**Mapped:** 2026-06-16
**Files analyzed:** 3 new files + 1 modification
**Analogs found:** 3 / 4

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|-------------------|------|-----------|----------------|---------------|
| `.gitlab/bench-analysis/fixtures/baseline.json` | config (static fixture) | batch | `/tmp/bench-artefacts/.gitlab/benchmarks/artifacts/baseline-v26-2.converted.json` | exact |
| `.gitlab/bench-analysis/fixtures/candidate.json` | config (static fixture) | batch | `/tmp/bench-artefacts/.gitlab/benchmarks/artifacts/candidate-v26-2.converted.json` | exact |
| `.gitlab/bench-analysis/preprocess.sh` | utility (CI script) | batch | `.gitlab/bench-analysis.yml` (script block) | role-match |
| `.gitlab/bench-analysis.yml` (modify: add step) | config (CI job) | request-response | `.gitlab/benchmarks.yml` | role-match |

## Pattern Assignments

### `.gitlab/bench-analysis/fixtures/baseline.json` (static fixture, batch)

**Analog:** `/tmp/bench-artefacts/.gitlab/benchmarks/artifacts/baseline-v26-2.converted.json`

**Top-level schema pattern:**
```json
{
  "schema_version": "v1",
  "benchmarks": [ ... ]
}
```

**Single benchmark entry structure** (copy this for every scenario):
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
      "execution_time": { "uom": "ns", "values": [12 floats] },
      "instructions":   { "uom": "instructions", "values": [12 floats] },
      "cpu_user_time":  { "uom": "ns", "values": [12 floats] },
      "max_rss_usage":  { "uom": "bytes", "values": [12 floats] }
    }
  }
}
```

**Baseline-specific parameter values:**

| Field | Value |
|-------|-------|
| `baseline_or_candidate` | `"baseline"` |
| `git_branch` | `"main"` |
| `git_commit_sha` | `"aaaaaaaabbbbbbbbccccccccdddddddd00000001"` |
| `git_commit_date` | `"1718000000"` |

**Four scenarios to include** (all 4 entries in one file):

| `name` | `variant` | `scenario` | Intent |
|--------|-----------|-----------|--------|
| `normalize` | `service` | `normalize-service-libdatadog` | regression (~20% slower in candidate) |
| `normalize` | `name` | `normalize-name-libdatadog` | unchanged (identical values) |
| `concentrator` | `add_spans` | `concentrator-libdatadog` | improvement (~15% faster in candidate) |
| `obfuscation` | `sql` | `obfuscation-sql-libdatadog` | noise (~0.3% delta, overlapping) |

**Raw value strategy** (12-value linear jitter: `base + i*step` for `i` in `[-5,-4,...,6]`):

| Scenario | Metric | Baseline base | Step |
|----------|--------|---------------|------|
| normalize-service | execution_time (ns) | 500,000 | 100 |
| normalize-name | execution_time (ns) | 400,000 | 100 |
| concentrator | execution_time (ns) | 5,000,000 | 500 |
| obfuscation-sql | execution_time (ns) | 100,000 | 100 |

For all scenarios: `cpu_user_time ≈ 99% of execution_time` (same step), `instructions` = proportional integer counts, `max_rss_usage` = flat array of the same realistic value (e.g., `2097152.0` = 2 MB).

---

### `.gitlab/bench-analysis/fixtures/candidate.json` (static fixture, batch)

**Analog:** `/tmp/bench-artefacts/.gitlab/benchmarks/artifacts/candidate-v26-2.converted.json`

Same structure as `baseline.json`. Only four `parameters` fields differ per entry:

| Field | candidate.json value |
|-------|---------------------|
| `baseline_or_candidate` | `"candidate"` |
| `git_branch` | `"pr-branch"` |
| `git_commit_sha` | `"bbbbbbbbccccccccddddddddeeeeeeee00000002"` |
| `git_commit_date` | `"1718000100"` |

**Candidate raw values** (same scenarios, different means):

| Scenario | Metric | Candidate base | Delta intent |
|----------|--------|----------------|-------------|
| normalize-service | execution_time (ns) | 600,000 | +20% (regression → `worse`) |
| normalize-name | execution_time (ns) | 400,000 | 0% (unchanged → `same`) |
| concentrator | execution_time (ns) | 4,250,000 | -15% (improvement → `better`) |
| obfuscation-sql | execution_time (ns) | 100,300 | +0.3% (noise → `same`/`unsure`) |

Use the same step as baseline per scenario. `instructions`, `cpu_user_time`, `max_rss_usage` scale proportionally from the new execution_time base.

---

### `.gitlab/bench-analysis/preprocess.sh` (utility script, batch)

**Analog:** `.gitlab/bench-analysis.yml` script block (lines 8-32) — the existing shell scripting style.

**Shebang and safety flags pattern** (from bench-analysis.yml style — always `set -euo pipefail`):
```bash
#!/usr/bin/env bash
set -euo pipefail
```

**Probe-before-use pattern** (fail fast with clear message):
```bash
command -v bp-analyzer || { echo "ERROR: bp-analyzer not found in PATH" >&2; exit 1; }
```

**Directory creation pattern** (from `benchmarks.yml` line 17):
```bash
mkdir -p artifacts
```

**Core bp-analyzer invocation pattern** (flags locked by D-05, D-07):
```bash
bp-analyzer compare pairwise \
  --baseline '{"git_branch":"main"}' \
  --candidate '{"git_branch":"pr-branch"}' \
  --format=md \
  --outpath=artifacts/benchmark-comparison.md \
  .gitlab/bench-analysis/fixtures/baseline.json \
  .gitlab/bench-analysis/fixtures/candidate.json
```

**Non-empty output assertion pattern:**
```bash
if [ ! -s artifacts/benchmark-comparison.md ]; then
  echo "ERROR: benchmark-comparison.md is empty — bp-analyzer produced no output" >&2
  exit 1
fi
echo "benchmark-comparison.md generated ($(wc -l < artifacts/benchmark-comparison.md) lines)"
```

---

### `.gitlab/bench-analysis.yml` (modify: add pre-processor step)

**Analog:** `.gitlab/bench-analysis.yml` lines 8-32 (existing script block)

**Insertion point:** After the `ANTHROPIC_CUSTOM_HEADERS` export (line 29) and before the smoke test (line 31). The pre-processor must run before Claude is invoked.

**Addition pattern** (one line, matches existing shell-invocation style):
```yaml
    - bash .gitlab/bench-analysis/preprocess.sh
```

No changes needed to `artifacts:` block — `artifacts/` path is already declared (line 33-35).

---

## Shared Patterns

### Shell Safety Header
**Source:** `.gitlab/bench-analysis.yml` + `.gitlab/benchmarks.yml` style  
**Apply to:** `preprocess.sh`
```bash
#!/usr/bin/env bash
set -euo pipefail
```

### Explicit `mkdir -p` Before File Output
**Source:** `.gitlab/benchmarks.yml` line 17: `mkdir "${ARTIFACTS_DIR}" || :`  
**Apply to:** `preprocess.sh` — use `mkdir -p artifacts` (stricter: no `|| :` since failure here is fatal)

### Separate Script File (not inline heredoc)
**Source:** `.gitlab/bench-analysis.yml` pattern — all multi-line logic is either in `|` blocks or external scripts.  
**Apply to:** `preprocess.sh` — committed as a separate file, called via `bash .gitlab/bench-analysis/preprocess.sh` from the YAML.

---

## No Analog Found

| File | Role | Data Flow | Reason |
|------|------|-----------|--------|
| (none) | — | — | All files have direct analogs in CI scripts or reference artifacts |

---

## Metadata

**Analog search scope:** `.gitlab/`, `/tmp/bench-artefacts/.gitlab/benchmarks/artifacts/`  
**Files scanned:** 3 (bench-analysis.yml, benchmarks.yml, reference artifacts)  
**Pattern extraction date:** 2026-06-16
