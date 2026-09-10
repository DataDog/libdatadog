#!/usr/bin/env python3
# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

"""Find Rust compiler errors present in a head build but absent from its base."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any


ANSI_ESCAPE = re.compile(r"\x1b\[[0-9;]*m")


def normalize_message(message: str, paths: list[Path]) -> str:
    """Replace candidate-specific absolute paths with stable placeholders."""
    for index, path in enumerate(paths):
        message = message.replace(str(path), f"<candidate-path-{index}>")
    return message


def error_fingerprints(
    messages_path: Path,
    log_path: Path | None = None,
    normalize_paths: list[Path] | None = None,
) -> set[tuple[str, str, str]]:
    """Return stable fingerprints for error-level Cargo compiler messages."""
    fingerprints: set[tuple[str, str, str]] = set()
    normalize_paths = normalize_paths or []
    with messages_path.open(encoding="utf-8") as messages:
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
            message = normalize_message(
                diagnostic.get("message", "unknown compiler error"), normalize_paths
            )
            fingerprints.add((target, code, message))

    # Dependency resolution and manifest failures happen before rustc emits JSON.
    # Use only their leading Cargo error lines, and only when no compiler
    # diagnostic exists, to avoid duplicating rendered rustc messages.
    if not fingerprints and log_path:
        with log_path.open(encoding="utf-8") as log:
            for line in log:
                normalized = ANSI_ESCAPE.sub("", line).strip()
                if normalized.startswith("error:"):
                    message = normalized.removeprefix("error:").strip()
                    fingerprints.add(
                        ("cargo", "", normalize_message(message, normalize_paths))
                    )
    return fingerprints


def new_errors(
    base_path: Path,
    head_path: Path,
    base_log: Path | None = None,
    head_log: Path | None = None,
    base_normalize_paths: list[Path] | None = None,
    head_normalize_paths: list[Path] | None = None,
) -> list[dict[str, str]]:
    base = error_fingerprints(base_path, base_log, base_normalize_paths)
    head = error_fingerprints(head_path, head_log, head_normalize_paths)
    return [
        {"target": target, "code": code, "message": message}
        for target, code, message in sorted(head - base)
    ]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", type=Path, required=True)
    parser.add_argument("--head", type=Path, required=True)
    parser.add_argument("--base-log", type=Path)
    parser.add_argument("--head-log", type=Path)
    parser.add_argument("--base-normalize-path", action="append", type=Path, default=[])
    parser.add_argument("--head-normalize-path", action="append", type=Path, default=[])
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--github-output", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if len(args.base_normalize_path) != len(args.head_normalize_path):
        raise ValueError(
            "--base-normalize-path and --head-normalize-path must be paired"
        )
    additional_errors = new_errors(
        args.base,
        args.head,
        args.base_log,
        args.head_log,
        args.base_normalize_path,
        args.head_normalize_path,
    )
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
