#!/usr/bin/env python3

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

"""Print a GitHub Actions workflow as JSON so bats tests can assert on it with jq.

Only a YAML-to-JSON transcription; no interpretation of the workflow itself.
"""

import json
import sys

import yaml


def normalize(node):
    """Undo YAML 1.1 boolean keys.

    `on:` — the trigger block every workflow has — is parsed as the boolean True by
    PyYAML, and so are `off:`/`yes:`/`no:`. Map those keys back to the words that
    were written in the file so queries can use them.
    """
    if isinstance(node, dict):
        out = {}
        for key, value in node.items():
            if key is True:
                key = "on"
            elif key is False:
                key = "off"
            elif not isinstance(key, str):
                key = str(key)
            out[key] = normalize(value)
        return out
    if isinstance(node, list):
        return [normalize(item) for item in node]
    return node


def main():
    if len(sys.argv) != 2:
        print(f"Usage: {sys.argv[0]} WORKFLOW_YAML", file=sys.stderr)
        return 2
    with open(sys.argv[1], encoding="utf-8") as handle:
        json.dump(normalize(yaml.safe_load(handle)), sys.stdout)
    return 0


if __name__ == "__main__":
    sys.exit(main())
