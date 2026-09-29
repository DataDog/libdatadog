#!/usr/bin/env bash
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
#
# Diff-scoped enforcement of .sg/rules/ffi-extern-c-panic-containment.yml.
# Matching is ast-grep; git only decides which signatures are new
# (hundreds of legacy accessors are left alone).
#
# Usage:
#   ./scripts/run-ffi-panic-lint.sh
#   ./scripts/run-ffi-panic-lint.sh --base-ref origin/main

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
RULE_ID="ffi-extern-c-panic-containment"

die() {
    echo "ERROR: $*" >&2
    exit 2
}

resolve_base() {
    if [[ "${1:-}" == "--base-ref" ]]; then
        local explicit="${2:-}"
        [[ -n "$explicit" ]] || die "--base-ref needs a ref"
        git rev-parse --verify "${explicit}^{commit}" >/dev/null 2>&1 \
            || die "unknown --base-ref ${explicit}"
        echo "$explicit"
        return
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
        git merge-base HEAD origin/main
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
# shellcheck disable=SC2064
trap "rm -f '$DIFF_FILE' '$FINDINGS_FILE'" EXIT
printf '%s\n' "$DIFF" >"$DIFF_FILE"

# Warning-severity rule: scan exits 0 even when it finds legacy hits.
# `--` skips the wrapper's GitHub-format injection so --json is legal.
bash "${SCRIPT_DIR}/run-ast-grep.sh" -- scan \
    --filter "$RULE_ID" \
    --json=compact \
    --color never >"$FINDINGS_FILE" || true

python3 - "$BASE" "$DIFF_FILE" "$FINDINGS_FILE" <<'PY'
import json, os, re, sys

base = sys.argv[1]
diff = open(sys.argv[2], encoding="utf-8").read()
findings_raw = open(sys.argv[3], encoding="utf-8").read()

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

if not findings_raw.strip() or findings_raw.strip() == "[]":
    print(f'ffi-panic-lint: no uncontained pub extern "C" functions vs {base}')
    sys.exit(0)

findings = json.loads(findings_raw)
hits = []
for finding in findings:
    path = finding.get("file") or finding.get("path")
    # 0-based JSON line of the `pub` / signature start — not the body.
    start = finding["range"]["start"]["line"] + 1
    if start in added.get(path, set()):
        hits.append((path, start))

if not hits:
    print(f'ffi-panic-lint: no new uncontained pub extern "C" functions vs {base}')
    sys.exit(0)

annotate = os.environ.get("GITHUB_ACTIONS") == "true"
message = (
    'new pub extern "C" function without panic containment; '
    "use wrap_with_ffi_result!, wrap_with_void_ffi_result!, catch_panic!, "
    "or std::panic::catch_unwind; if containment is redundant, add "
    "`// allow(ffi-panic-boundary): <justification>` above the function"
)
for path, start in hits:
    print(f"{path}:{start}: {message}")
    if annotate:
        print(f"::error file={path},line={start}::{message}")

print(
    f'\nffi-panic-lint: {len(hits)} new uncontained pub extern "C" '
    f"function(s) vs {base}",
    file=sys.stderr,
)
sys.exit(1)
PY
