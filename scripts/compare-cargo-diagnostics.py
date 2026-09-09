#!/usr/bin/env python3
# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

"""Find Rust compiler errors present in a head build but absent from its base."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any


def error_fingerprints(path: Path) -> set[tuple[str, str, str]]:
    """Return stable fingerprints for error-level Cargo compiler messages."""
    fingerprints: set[tuple[str, str, str]] = set()
    with path.open(encoding="utf-8") as messages:
        for line in messages:
            try:
                cargo_message: dict[str, Any] = json.loads(line)
            except json.JSONDecodeError:
                continue
            if cargo_message.get("reason") != "compiler-message":
                continue
            diagnostic = cargo_message.get("message", {})
            if diagnostic.get("level") != "error":
                continue
            target = cargo_message.get("target", {}).get("name", "unknown-target")
            code = (diagnostic.get("code") or {}).get("code", "")
            message = diagnostic.get("message", "unknown compiler error")
            fingerprints.add((target, code, message))
    return fingerprints


def new_errors(base_path: Path, head_path: Path) -> list[dict[str, str]]:
    base = error_fingerprints(base_path)
    head = error_fingerprints(head_path)
    return [
        {"target": target, "code": code, "message": message}
        for target, code, message in sorted(head - base)
    ]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", type=Path, required=True)
    parser.add_argument("--head", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--github-output", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    additional_errors = new_errors(args.base, args.head)
    args.output.write_text(json.dumps(additional_errors, indent=2) + "\n", encoding="utf-8")
    if args.github_output:
        with args.github_output.open("a", encoding="utf-8") as output:
            output.write(f"new_error_count={len(additional_errors)}\n")
    else:
        print(len(additional_errors))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except OSError as error:
        print(f"compare-cargo-diagnostics: {error}", file=sys.stderr)
        sys.exit(2)
