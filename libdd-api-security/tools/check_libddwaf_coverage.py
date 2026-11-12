#!/usr/bin/env python3
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
"""Audit named test mappings; exit nonzero until every reference case is mapped."""
import argparse
import hashlib
import json
import re
from pathlib import Path

crate = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser()
parser.add_argument("reference", type=Path, help="libddwaf source checkout")
parser.add_argument("--test-list", type=Path, help="cargo nextest list --message-format json output")
args = parser.parse_args()
inventory = json.loads((crate / "tests/fixtures/libddwaf-coverage.json").read_text())
test_source = (crate / "tests/libddwaf.rs").read_text()
compiled = None
if args.test_list:
    compiled = set()
    for suite in json.loads(args.test_list.read_text())["rust-suites"].values():
        compiled.update(suite["binary-name"] + "::" + name for name in suite["testcases"])
errors = []
missing = []
for entry in inventory["tests"]:
    path = args.reference / entry["source"]
    content = path.read_bytes()
    if hashlib.sha256(content).hexdigest() != entry["source_sha256"]:
        errors.append(f"reference changed: {entry['source']}")
    suite, name = entry["reference"].split(".")
    if not re.search(r"TEST\(" + re.escape(suite) + r",\s*" + re.escape(name) + r"\)", content.decode()):
        errors.append(f"reference test not found: {entry['reference']}")
    mapped = entry["rust_test"]
    if mapped is None:
        missing.append(entry["reference"])
    elif compiled is not None and mapped not in compiled:
        errors.append(f"mapped test absent from compiled inventory: {mapped}")
    elif mapped.rsplit("::", 1)[1] not in test_source:
        errors.append(f"mapped Rust test not found: {mapped}")
for resource in inventory.get("resources", []):
    if hashlib.sha256((args.reference / resource["source"]).read_bytes()).hexdigest() != resource["source_sha256"]:
        errors.append(f"reference resource changed: {resource['source']}")
print(f"{len(inventory['tests']) - len(missing)}/{len(inventory['tests'])} reference tests mapped")
print("Compiled mappings verified." if compiled is not None else "Pass --test-list to also verify mappings against compiled tests.")
print("Run cargo nextest to verify the assertions.")
for error in sorted(set(errors)):
    print(error)
if missing:
    print(f"{len(missing)} cases still unported")
raise SystemExit(1 if errors or missing else 0)
