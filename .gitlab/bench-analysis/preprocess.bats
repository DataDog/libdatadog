#!/usr/bin/env bats
# Smoke test suite for the bench-analysis pre-processor pipeline.
# Non-pipeline tests (JSON validity, schema, scenarios, metrics) run everywhere.
# Pipeline tests (preprocess.sh execution, scenario names in output) require
# bp-analyzer in PATH and are skipped locally.

REPO_ROOT="$(cd "${BATS_TEST_DIRNAME}/../.." && pwd)"
FIXTURE_DIR="$REPO_ROOT/.gitlab/bench-analysis/fixtures"
BASELINE="$FIXTURE_DIR/baseline.json"
CANDIDATE="$FIXTURE_DIR/candidate.json"
PREPROCESS_SH="$REPO_ROOT/.gitlab/bench-analysis/preprocess.sh"
COMPARISON_OUT="$REPO_ROOT/artifacts/benchmark-comparison.md"

SCENARIOS=(
  "normalize-service-libdatadog"
  "normalize-name-libdatadog"
  "concentrator-libdatadog"
  "obfuscation-sql-libdatadog"
)

setup() {
  rm -f "$COMPARISON_OUT"
}

@test "valid JSON: baseline.json and candidate.json parse without error" {
  python3 -c "import json; json.load(open('$BASELINE'))"
  python3 -c "import json; json.load(open('$CANDIDATE'))"
}

@test "BP v1 schema: both fixtures have schema_version==v1 and non-empty benchmarks array" {
  python3 -c "
import json
for path in ['$BASELINE', '$CANDIDATE']:
    d = json.load(open(path))
    assert d.get('schema_version') == 'v1', f'{path}: schema_version != v1'
    assert len(d.get('benchmarks', [])) > 0, f'{path}: benchmarks array is empty'
"
}

@test "four scenarios present: each fixture contains exactly the four required scenario names" {
  python3 -c "
import json
expected = {'normalize-service-libdatadog', 'normalize-name-libdatadog', 'concentrator-libdatadog', 'obfuscation-sql-libdatadog'}
for path in ['$BASELINE', '$CANDIDATE']:
    d = json.load(open(path))
    actual = {b['parameters']['scenario'] for b in d['benchmarks']}
    assert actual == expected, f'{path}: scenarios mismatch. got={actual}'
"
}

@test "four metrics 12 values: every runs[#1] has the four metrics each with 12-element values array" {
  python3 -c "
import json
metrics = ['execution_time', 'instructions', 'cpu_user_time', 'max_rss_usage']
for path in ['$BASELINE', '$CANDIDATE']:
    d = json.load(open(path))
    for b in d['benchmarks']:
        scenario = b['parameters']['scenario']
        run = b['runs']['#1']
        for m in metrics:
            assert m in run, f'{path} {scenario}: missing metric {m}'
            vals = run[m].get('values', [])
            assert len(vals) == 12, f'{path} {scenario} {m}: expected 12 values, got {len(vals)}'
"
}

@test "non-empty comparison: preprocess.sh exits 0 and benchmark-comparison.md is non-empty" {
  { command -v bp-analyzer || [ -x /opt/dogbrew/bin/bp-analyzer ]; } >/dev/null 2>&1 || skip "bp-analyzer not available (CI-only)"
  bash "$PREPROCESS_SH"
  [ -s "$COMPARISON_OUT" ]
}

@test "comparison names scenarios: output contains all four scenario strings" {
  { command -v bp-analyzer || [ -x /opt/dogbrew/bin/bp-analyzer ]; } >/dev/null 2>&1 || skip "bp-analyzer not available (CI-only)"
  [ -s "$COMPARISON_OUT" ] || bash "$PREPROCESS_SH"
  for scenario in "${SCENARIOS[@]}"; do
    grep -q "$scenario" "$COMPARISON_OUT"
  done
}
