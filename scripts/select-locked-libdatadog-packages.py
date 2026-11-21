#!/usr/bin/env python3
# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

"""Select locked libdatadog packages that need a targeted Cargo update."""

from __future__ import annotations

import argparse
import sys
import tomllib
from collections import Counter
from pathlib import Path


def load_libdatadog_package_names(path: Path) -> set[str]:
    names = set()
    lines = path.read_text(encoding="utf-8").splitlines()
    for line_number, line in enumerate(lines, 1):
        if not line:
            continue
        fields = line.split("\t")
        if len(fields) != 2 or not fields[0]:
            raise ValueError(f"invalid package entry on line {line_number} of {path}")
        names.add(fields[0])
    return names


def normalize_git_url(url: str) -> str:
    return url.rstrip("/").removesuffix(".git")


def source_matches(source: str | None, patch_sources: list[str]) -> bool:
    if source is None:
        return False
    if source in {
        "registry+https://github.com/rust-lang/crates.io-index",
        "sparse+https://index.crates.io/",
    }:
        return "crates-io" in patch_sources
    if not source.startswith("git+"):
        return False

    locked_url = source.removeprefix("git+").split("#", 1)[0].split("?", 1)[0]
    normalized_locked_url = normalize_git_url(locked_url)
    return any(
        patch_source != "crates-io"
        and normalize_git_url(patch_source) == normalized_locked_url
        for patch_source in patch_sources
    )


def select_package_specs(
    lockfile: Path, package_names: set[str], patch_sources: list[str]
) -> list[str]:
    if not lockfile.exists():
        return []

    with lockfile.open("rb") as stream:
        lock_data = tomllib.load(stream)

    candidates = [
        (package["name"], package["version"])
        for package in lock_data.get("package", [])
        if package.get("name") in package_names
        and source_matches(package.get("source"), patch_sources)
    ]
    counts = Counter(candidates)
    duplicates = sorted(
        f"{name}@{version}"
        for (name, version), count in counts.items()
        if count > 1
    )
    if duplicates:
        raise ValueError(
            "ambiguous locked package specs from multiple patched sources: "
            + ", ".join(duplicates)
        )
    return sorted(f"{name}@{version}" for name, version in candidates)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lockfile", type=Path, required=True)
    parser.add_argument("--packages", type=Path, required=True)
    parser.add_argument("--patch-source", action="append", default=[])
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        package_names = load_libdatadog_package_names(args.packages)
        specs = select_package_specs(args.lockfile, package_names, args.patch_source)
    except (OSError, KeyError, tomllib.TOMLDecodeError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2

    for spec in specs:
        print(spec)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
