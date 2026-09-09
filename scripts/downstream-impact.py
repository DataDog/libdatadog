#!/usr/bin/env python3
# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

"""Map a libdatadog change set to potentially impacted downstream repositories."""

from __future__ import annotations

import argparse
import fnmatch
import json
import subprocess
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any


def load_config(path: Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as config_file:
        config = json.load(config_file)

    if config.get("schema_version") != 1:
        raise ValueError("unsupported downstream consumer schema version")

    consumers = config.get("consumers", [])
    components = config.get("components", [])
    repositories = [consumer.get("repository") for consumer in consumers]
    if not repositories or any(not repository for repository in repositories):
        raise ValueError("every consumer must have a repository")
    if len(repositories) != len(set(repositories)):
        raise ValueError("consumer repositories must be unique")

    known_repositories = set(repositories)
    component_names: set[str] = set()
    for component in components:
        name = component.get("name")
        if not name or name in component_names:
            raise ValueError("component names must be present and unique")
        component_names.add(name)
        if not component.get("paths"):
            raise ValueError(f"component {name!r} must define paths")
        unknown = set(component.get("consumers", [])) - known_repositories - {"*"}
        if unknown:
            raise ValueError(f"component {name!r} has unknown consumers: {sorted(unknown)}")

    return config


def git_changed_files(base: str, head: str) -> list[str]:
    result = subprocess.run(
        ["git", "diff", "--name-only", "--diff-filter=ACMR", f"{base}...{head}"],
        check=True,
        capture_output=True,
        text=True,
    )
    return [line for line in result.stdout.splitlines() if line]


def calculate_impact(config: dict[str, Any], changed_files: list[str]) -> dict[str, Any]:
    consumers_by_repository = {
        consumer["repository"]: consumer for consumer in config["consumers"]
    }
    matches_by_repository: dict[str, dict[str, set[str]]] = defaultdict(
        lambda: defaultdict(set)
    )

    for component in config["components"]:
        matching_paths = {
            path
            for path in changed_files
            if any(fnmatch.fnmatchcase(path, pattern) for pattern in component["paths"])
        }
        if not matching_paths:
            continue

        repositories = component["consumers"]
        if repositories == ["*"] or "*" in repositories:
            repositories = list(consumers_by_repository)
        for repository in repositories:
            matches_by_repository[repository][component["name"]].update(matching_paths)

    impacted = []
    for repository in sorted(matches_by_repository):
        consumer = consumers_by_repository[repository]
        component_matches = matches_by_repository[repository]
        impacted.append(
            {
                "repository": repository,
                "tier": consumer["tier"],
                "mode": consumer["mode"],
                "components": sorted(component_matches),
                "changed_files": sorted(
                    {path for paths in component_matches.values() for path in paths}
                ),
            }
        )

    return {
        "schema_version": 1,
        "changed_files": sorted(set(changed_files)),
        "impacted_count": len(impacted),
        "total_consumers": len(consumers_by_repository),
        "impacted": impacted,
        "matrix": {
            "include": [
                {
                    "repository": item["repository"],
                    "tier": item["tier"],
                    "mode": item["mode"],
                    "components": ",".join(item["components"]),
                }
                for item in impacted
            ]
        },
    }


def markdown_report(impact: dict[str, Any]) -> str:
    lines = ["## Downstream impact analysis", ""]
    if not impact["impacted"]:
        lines.extend(
            [
                "No configured downstream consumer is potentially impacted by this change set.",
                "",
                "> This is a static mapping result; it does not prove runtime compatibility.",
            ]
        )
        return "\n".join(lines) + "\n"

    lines.extend(
        [
            f"{impact['impacted_count']} of {impact['total_consumers']} configured repositories "
            "are potentially impacted.",
            "",
            "| Repository | Tier | Validation mode | Components |",
            "|---|---|---|---|",
        ]
    )
    for item in impact["impacted"]:
        components = ", ".join(f"`{name}`" for name in item["components"])
        lines.append(
            f"| `{item['repository']}` | {item['tier']} | {item['mode']} | {components} |"
        )
    lines.extend(
        [
            "",
            "> Potential impact is selected from the versioned component mapping. "
            "Downstream build jobs will consume the generated matrix in a follow-up phase.",
        ]
    )
    return "\n".join(lines) + "\n"


def write_github_outputs(path: Path, impact: dict[str, Any]) -> None:
    with path.open("a", encoding="utf-8") as output_file:
        output_file.write(f"has_impacted={'true' if impact['impacted'] else 'false'}\n")
        output_file.write(f"impacted_count={impact['impacted_count']}\n")
        output_file.write(f"matrix={json.dumps(impact['matrix'], separators=(',', ':'))}\n")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--config",
        type=Path,
        default=Path(".github/downstream-consumers.json"),
    )
    parser.add_argument("--base", help="base Git revision for a three-dot diff")
    parser.add_argument("--head", help="head Git revision for a three-dot diff")
    parser.add_argument("--changed-file", action="append", default=[])
    parser.add_argument("--json-output", type=Path)
    parser.add_argument("--markdown-output", type=Path)
    parser.add_argument("--github-output", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if bool(args.base) != bool(args.head):
        raise ValueError("--base and --head must be provided together")
    if args.base and args.changed_file:
        raise ValueError("use either Git revisions or --changed-file, not both")

    changed_files = (
        git_changed_files(args.base, args.head) if args.base else args.changed_file
    )
    impact = calculate_impact(load_config(args.config), changed_files)
    json_report = json.dumps(impact, indent=2) + "\n"
    markdown = markdown_report(impact)

    if args.json_output:
        args.json_output.write_text(json_report, encoding="utf-8")
    else:
        sys.stdout.write(json_report)
    if args.markdown_output:
        args.markdown_output.write_text(markdown, encoding="utf-8")
    if args.github_output:
        write_github_outputs(args.github_output, impact)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"downstream-impact: {error}", file=sys.stderr)
        sys.exit(2)
