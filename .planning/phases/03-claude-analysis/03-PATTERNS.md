# Phase 3: Claude Analysis - Pattern Map

**Mapped:** 2026-06-17
**Files analyzed:** 3 new files + 1 modified
**Analogs found:** 3 / 3

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|-------------------|------|-----------|----------------|---------------|
| `.gitlab/bench-analysis/analyze.sh` | utility (CI script) | request-response | `.gitlab/bench-analysis/preprocess.sh` | exact |
| `.gitlab/bench-analysis/analyze-prompt.md` | config (system prompt) | — | `.planning/phases/03-claude-analysis/03-RESEARCH.md §Pattern 4` | no codebase analog |
| `.gitlab/bench-analysis/analyze.bats` | test | request-response | `.gitlab/bench-analysis/preprocess.bats` | exact |
| `.gitlab/bench-analysis.yml` | config (CI job) | request-response | `.gitlab/bench-analysis.yml` (self, Phase 1) | exact (modification) |

## Pattern Assignments

### `.gitlab/bench-analysis/analyze.sh` (utility, request-response)

**Analog:** `.gitlab/bench-analysis/preprocess.sh`

**Shebang + strict mode** (lines 1-2):
```bash
#!/usr/bin/env bash
set -euo pipefail
```

**SCRIPT_DIR resolution pattern** — absent in preprocess.sh but required here for `--system-prompt-file`; derive from BATS pattern in preprocess.bats (line 7):
```bash
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
```

**Env-var-overridable path defaults** (preprocess.sh lines 6-9):
```bash
BASELINE_JSON="${BASELINE_JSON:-.gitlab/bench-analysis/fixtures/baseline.json}"
CANDIDATE_JSON="${CANDIDATE_JSON:-.gitlab/bench-analysis/fixtures/candidate.json}"
```
Copy this pattern for:
```bash
PROMPT_FILE="${PROMPT_FILE:-${SCRIPT_DIR}/analyze-prompt.md}"
COMPARISON="${COMPARISON:-artifacts/benchmark-comparison.md}"
REPORT="${REPORT:-artifacts/benchmark-report.md}"
```

**Pre-condition guard** (preprocess.sh lines 4, 20-23):
```bash
command -v bp-analyzer >/dev/null || { echo "ERROR: bp-analyzer not found in PATH" >&2; exit 1; }
```
Copy pattern for missing comparison file:
```bash
if [ ! -s "${COMPARISON}" ]; then
  echo "ERROR: ${COMPARISON} is missing or empty — run preprocess.sh first" >&2
  exit 1
fi
```

**NVM sourcing in non-interactive shell** — from bench-analysis.yml lines 18-22 (Phase 1 proven):
```bash
export NVM_DIR="$HOME/.nvm"
# shellcheck source=/dev/null
[ -s "$NVM_DIR/nvm.sh" ] && . "$NVM_DIR/nvm.sh"
```

**Claude `--bare -p` invocation** — from bench-analysis.yml line 36 (Phase 1 proven):
```bash
claude --bare -p '...' --model anthropic/claude-sonnet-4-6 --allowedTools 'Read' --permission-mode bypassPermissions
```
Phase 3 extends this with `--allowedTools "Read,Write"` and `--system-prompt-file`.

**Non-empty output assertion** (preprocess.sh lines 20-23):
```bash
if [ ! -s artifacts/benchmark-comparison.md ]; then
  echo "ERROR: benchmark-comparison.md is empty — bp-analyzer produced no output" >&2
  exit 1
fi
echo "benchmark-comparison.md generated ($(wc -l < artifacts/benchmark-comparison.md) lines)"
```
Copy this verbatim, substituting `${REPORT}` and updating the error message.

---

### `.gitlab/bench-analysis/analyze.bats` (test, request-response)

**Analog:** `.gitlab/bench-analysis/preprocess.bats`

**Shebang + file-level comment** (lines 1-6):
```bash
#!/usr/bin/env bats
# Smoke test suite for the bench-analysis pre-processor pipeline.
# Non-pipeline tests (...) run everywhere.
# Pipeline tests (...) require bp-analyzer in PATH and are skipped locally.
```

**REPO_ROOT + path constants** (lines 7-12):
```bash
REPO_ROOT="$(cd "${BATS_TEST_DIRNAME}/../.." && pwd)"
FIXTURE_DIR="$REPO_ROOT/.gitlab/bench-analysis/fixtures"
PREPROCESS_SH="$REPO_ROOT/.gitlab/bench-analysis/preprocess.sh"
COMPARISON_OUT="$REPO_ROOT/artifacts/benchmark-comparison.md"
```
Copy for analyze.bats:
```bash
REPO_ROOT="$(cd "${BATS_TEST_DIRNAME}/../.." && pwd)"
ANALYZE_SH="$REPO_ROOT/.gitlab/bench-analysis/analyze.sh"
PROMPT_FILE="$REPO_ROOT/.gitlab/bench-analysis/analyze-prompt.md"
REPORT_OUT="$REPO_ROOT/artifacts/benchmark-report.md"
COMPARISON_OUT="$REPO_ROOT/artifacts/benchmark-comparison.md"
```

**setup() to clear stale artifact** (lines 21-23):
```bash
setup() {
  rm -f "$COMPARISON_OUT"
}
```
Copy for analyze.bats clearing `$REPORT_OUT`.

**CI-only skip guard** (lines 67-70):
```bash
@test "non-empty comparison: preprocess.sh exits 0 and benchmark-comparison.md is non-empty" {
  command -v bp-analyzer >/dev/null || skip "bp-analyzer not available (CI-only)"
  bash "$PREPROCESS_SH"
  [ -s "$COMPARISON_OUT" ]
}
```
Copy this pattern for the analyze.sh integration test, skipping when `claude` is not in PATH:
```bash
@test "analyze.sh produces non-empty benchmark-report.md" {
  command -v claude >/dev/null || skip "claude not available (CI-only)"
  [ -s "$COMPARISON_OUT" ] || skip "benchmark-comparison.md missing — run preprocess.sh first"
  bash "$ANALYZE_SH"
  [ -s "$REPORT_OUT" ]
}
```

---

### `.gitlab/bench-analysis.yml` (modification — add analyze.sh step)

**Analog:** `.gitlab/bench-analysis.yml` (self)

**Insertion point** (line 34-36):
```yaml
    - bash .gitlab/bench-analysis/preprocess.sh
    # Smoke test (D-09, CI-04)
    - "claude --bare -p 'Read the root Cargo.toml and tell me the workspace version.' --model anthropic/claude-sonnet-4-6 --allowedTools 'Read' --permission-mode bypassPermissions"
```
Replace the smoke test line with:
```yaml
    - bash .gitlab/bench-analysis/preprocess.sh
    - bash .gitlab/bench-analysis/analyze.sh
```

---

## Shared Patterns

### NVM sourcing
**Source:** `.gitlab/bench-analysis.yml` lines 18-22
**Apply to:** `analyze.sh`
```bash
export NVM_DIR="$HOME/.nvm"
. "$NVM_DIR/nvm.sh"
```

### Non-empty file assertion
**Source:** `.gitlab/bench-analysis/preprocess.sh` lines 20-23
**Apply to:** `analyze.sh` (for `$REPORT`), `analyze.bats` (for post-run check)
```bash
if [ ! -s <output_file> ]; then
  echo "ERROR: <output_file> is empty — <tool> produced no output" >&2
  exit 1
fi
echo "<output_file> generated ($(wc -l < <output_file>) lines)"
```

### CI-only skip guard
**Source:** `.gitlab/bench-analysis/preprocess.bats` lines 67-68
**Apply to:** `analyze.bats` for any test requiring `claude` in PATH
```bash
command -v <tool> >/dev/null || skip "<tool> not available (CI-only)"
```

### REPO_ROOT via BATS_TEST_DIRNAME
**Source:** `.gitlab/bench-analysis/preprocess.bats` line 7
**Apply to:** `analyze.bats`
```bash
REPO_ROOT="$(cd "${BATS_TEST_DIRNAME}/../.." && pwd)"
```

## No Analog Found

| File | Role | Data Flow | Reason |
|------|------|-----------|--------|
| `.gitlab/bench-analysis/analyze-prompt.md` | config (system prompt) | — | No existing system prompt files in codebase; use RESEARCH.md §Pattern 4 as template |

## Metadata

**Analog search scope:** `.gitlab/bench-analysis/`, `.gitlab/bench-analysis.yml`
**Files scanned:** 4
**Pattern extraction date:** 2026-06-17
