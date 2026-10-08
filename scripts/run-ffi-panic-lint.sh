#!/usr/bin/env bash
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
#
# Diff-scoped enforcement of the FFI panic-containment ast-grep rules.
#
# ffi-extern-c-panic-containment matches function_item nodes. About 400
# existing accessors would fail a whole-repo error scan; we do not
# rewrite them. This script intersects those hits with git diff -U0
# *signature* lines (the `pub` line).
#
# ast-grep does not expand macros, so it never sees `pub extern "C"`
# functions that exist only in a token tree (libdd-telemetry-ffi
# `c_setters!`, used from builder/macros.rs when
# `expanded_builder_macros` is off). ffi-macro-emits-extern-c matches
# those macro_rules templates. This script also treats invocations of
# those macros as generating sites: an added line inside `c_setters!(…)`
# (a new setter) fails even though no function_item appeared.
#
# `run-ast-grep.sh` leaves both rules `--off` on the whole-repo scan.
#
# Usage:
#   ./scripts/run-ffi-panic-lint.sh
#   ./scripts/run-ffi-panic-lint.sh --base-ref origin/main
#   FFI_PANIC_LINT_BASE=<sha> ./scripts/run-ffi-panic-lint.sh
#
# On GitHub push events, lint.yml sets FFI_PANIC_LINT_BASE to
# github.event.before. After actions/checkout, origin/main is the new
# HEAD, so merge-base HEAD origin/main is empty and would skip the scan.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
FN_RULE="ffi-extern-c-panic-containment"
MACRO_RULE="ffi-macro-emits-extern-c"
# GitHub's sentinel for "this ref did not exist before the push".
ZERO_SHA="0000000000000000000000000000000000000000"

die() {
    echo "ERROR: $*" >&2
    exit 2
}

ensure_commit() {
    local ref="$1"
    if git rev-parse --verify "${ref}^{commit}" >/dev/null 2>&1; then
        return 0
    fi
    git fetch --no-tags --depth=1 origin "$ref" \
        || die "failed to fetch ${ref}"
    git rev-parse --verify "${ref}^{commit}" >/dev/null 2>&1 \
        || die "unknown commit ${ref}"
}

# Pre-push SHA: workflow env, else the push event payload.
push_before_sha() {
    local before="${FFI_PANIC_LINT_BASE:-}"
    if [[ -z "$before" && -n "${GITHUB_EVENT_PATH:-}" && -f "${GITHUB_EVENT_PATH}" ]]; then
        before="$(python3 -c 'import json, sys
print(json.load(open(sys.argv[1], encoding="utf-8")).get("before") or "")' \
            "${GITHUB_EVENT_PATH}")"
    fi
    printf '%s' "$before"
}

resolve_base() {
    if [[ "${1:-}" == "--base-ref" ]]; then
        local explicit="${2:-}"
        [[ -n "$explicit" ]] || die "--base-ref needs a ref"
        ensure_commit "$explicit"
        echo "$explicit"
        return
    fi

    if [[ "${GITHUB_EVENT_NAME:-}" == "push" ]]; then
        local before
        before="$(push_before_sha)"
        if [[ -n "$before" && "$before" != "$ZERO_SHA" ]]; then
            ensure_commit "$before"
            echo "$before"
            return
        fi
        # New ref (before is the zero SHA): fall through to origin/main.
    fi

    if [[ "${GITHUB_EVENT_NAME:-}" == "pull_request" && -n "${GITHUB_BASE_REF:-}" ]]; then
        local remote_base="origin/${GITHUB_BASE_REF}"
        if ! git rev-parse --verify "${remote_base}^{commit}" >/dev/null 2>&1; then
            git fetch --no-tags --depth=1 origin "${GITHUB_BASE_REF}" \
                || die "failed to fetch origin/${GITHUB_BASE_REF}"
        fi
        echo "$remote_base"
        return
    fi

    if git rev-parse --verify origin/main >/dev/null 2>&1; then
        local mb head before
        mb="$(git merge-base HEAD origin/main)"
        head="$(git rev-parse HEAD)"
        before="$(push_before_sha)"
        # Push to main after checkout: origin/main is HEAD. Without a
        # pre-push SHA the diff is empty and the scan never runs.
        if [[ "${GITHUB_EVENT_NAME:-}" == "push" && "$mb" == "$head" && -z "$before" ]]; then
            die "push event has no pre-push SHA; set FFI_PANIC_LINT_BASE to github.event.before"
        fi
        echo "$mb"
        return
    fi

    echo ""
}

cd "$ROOT_DIR"

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo "ffi-panic-lint: not a git checkout, skipping diff-scoped check"
    exit 0
fi

BASE="$(resolve_base "${@:-}")"
if [[ -z "$BASE" ]]; then
    echo "ffi-panic-lint: no merge-base (fetch origin/main to enable), skipping"
    exit 0
fi

DIFF="$(git diff -U0 --diff-filter=ACMR "$BASE" -- '*.rs' || true)"
if [[ -z "$DIFF" ]]; then
    echo "ffi-panic-lint: no Rust diff vs ${BASE}"
    exit 0
fi

DIFF_FILE="$(mktemp)"
FINDINGS_FILE="$(mktemp)"
INVOCATIONS_FILE="$(mktemp)"
# shellcheck disable=SC2064
trap "rm -f '$DIFF_FILE' '$FINDINGS_FILE' '$INVOCATIONS_FILE'" EXIT
printf '%s\n' "$DIFF" >"$DIFF_FILE"

# Warning-severity rules: scan exits 0 even when it finds legacy hits.
# `--` skips the wrapper's GitHub-format injection so --json is legal.
# Do not swallow a non-zero exit: a scanner/config failure would otherwise
# leave empty JSON and look like "no uncontained functions".
if ! bash "${SCRIPT_DIR}/run-ast-grep.sh" -- scan \
    --filter "${FN_RULE}|${MACRO_RULE}" \
    --json=compact \
    --color never >"$FINDINGS_FILE"; then
    die "ast-grep scan of ${FN_RULE}|${MACRO_RULE} failed"
fi

python3 - "$BASE" "$DIFF_FILE" "$FINDINGS_FILE" "$INVOCATIONS_FILE" \
    "$SCRIPT_DIR/run-ast-grep.sh" "$FN_RULE" "$MACRO_RULE" <<'PY'
import json, os, re, subprocess, sys, tempfile

base = sys.argv[1]
diff = open(sys.argv[2], encoding="utf-8").read()
findings_raw = open(sys.argv[3], encoding="utf-8").read()
invocations_path = sys.argv[4]
runner = sys.argv[5]
fn_rule = sys.argv[6]
macro_rule = sys.argv[7]

added = {}
current = None
new_line = None
hunk = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@")
for line in diff.splitlines():
    if line.startswith("+++ "):
        path = line[4:]
        if path == "/dev/null":
            current = None
            continue
        if path.startswith("b/"):
            path = path[2:]
        current = path
        added.setdefault(current, set())
        continue
    match = hunk.match(line)
    if match:
        new_line = int(match.group(1))
        continue
    if current is None or new_line is None:
        continue
    if line.startswith("+"):
        added[current].add(new_line)
        new_line += 1
    elif not line.startswith("-") and not line.startswith("\\"):
        new_line += 1


def load_json(raw):
    raw = raw.strip()
    if not raw:
        print("ERROR: ast-grep produced no JSON", file=sys.stderr)
        sys.exit(2)
    return json.loads(raw)


def range_lines(finding):
    start = finding["range"]["start"]["line"] + 1
    end = finding["range"]["end"]["line"] + 1
    return start, end


def added_in_range(path, start, end):
    lines = added.get(path, set())
    return any(line in lines for line in range(start, end + 1))


findings = load_json(findings_raw)
macro_names = set()
for finding in findings:
    if finding.get("ruleId") != macro_rule:
        continue
    match = re.search(r"macro_rules!\s+(\w+)", finding.get("text", ""))
    if match:
        macro_names.add(match.group(1))

if macro_names:
    alt = "|".join(re.escape(name) for name in sorted(macro_names))
    rule = f"""
id: ffi-macro-invocation-emits-extern-c
language: rust
rule:
  kind: macro_invocation
  regex: "(^|::)({alt})!"
"""
    with tempfile.NamedTemporaryFile("w", suffix=".yml", delete=False) as handle:
        handle.write(rule)
        rule_path = handle.name
    try:
        proc = subprocess.run(
            [
                "bash",
                runner,
                "--",
                "scan",
                "--rule",
                rule_path,
                "--json=compact",
                "--color",
                "never",
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        if proc.returncode not in (0, 1):
            sys.stderr.write(proc.stderr)
            sys.exit(2)
        open(invocations_path, "w", encoding="utf-8").write(proc.stdout)
    finally:
        os.unlink(rule_path)
else:
    open(invocations_path, "w", encoding="utf-8").write("[]")

invocations = load_json(open(invocations_path, encoding="utf-8").read())

hits = []
for finding in findings:
    path = finding.get("file") or finding.get("path")
    start, end = range_lines(finding)
    rule_id = finding.get("ruleId")
    if rule_id == fn_rule:
        if start in added.get(path, set()):
            hits.append((path, start, "function"))
    elif rule_id == macro_rule:
        if added_in_range(path, start, end):
            hits.append((path, start, "macro"))

for finding in invocations:
    path = finding.get("file") or finding.get("path")
    start, end = range_lines(finding)
    if added_in_range(path, start, end):
        hits.append((path, start, "invocation"))

# Stable unique (path, line) so a new setter does not double-report.
seen = set()
unique = []
for path, start, kind in hits:
    key = (path, start)
    if key in seen:
        continue
    seen.add(key)
    unique.append((path, start, kind))

if not unique:
    print(f'ffi-panic-lint: no new uncontained pub extern "C" FFI vs {base}')
    sys.exit(0)

annotate = os.environ.get("GITHUB_ACTIONS") == "true"
messages = {
    "function": (
        'new pub extern "C" function without panic containment; '
        "use wrap_with_ffi_result!, wrap_with_void_ffi_result!, catch_panic!, "
        "or std::panic::catch_unwind; if containment is redundant, add "
        "`// allow(ffi-panic-boundary): <justification>` above the function"
    ),
    "macro": (
        "this macro_rules emits pub extern \"C\" functions without panic "
        "containment; wrap the generated bodies or add "
        "`// allow(ffi-panic-boundary): <justification>` above the macro. "
        "ast-grep does not expand macros"
    ),
    "invocation": (
        "new tokens in an invocation of a macro that emits uncontained "
        "pub extern \"C\" functions (e.g. c_setters!); put catch_unwind / "
        "wrap_with_ffi_result! / catch_panic! in the macro template, or add "
        "`// allow(ffi-panic-boundary): <justification>` above the macro_rules"
    ),
}
for path, start, kind in unique:
    message = messages[kind]
    print(f"{path}:{start}: {message}")
    if annotate:
        print(f"::error file={path},line={start}::{message}")

print(
    f"\nffi-panic-lint: {len(unique)} new uncontained pub extern \"C\" "
    f"FFI site(s) vs {base}",
    file=sys.stderr,
)
sys.exit(1)
PY
