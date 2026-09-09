# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT_PATH = ROOT / "scripts" / "compare-cargo-diagnostics.py"
SPEC = importlib.util.spec_from_file_location("compare_cargo_diagnostics", SCRIPT_PATH)
assert SPEC and SPEC.loader
compare = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(compare)


def compiler_message(target: str, message: str, code: str | None = None) -> str:
    return json.dumps(
        {
            "reason": "compiler-message",
            "target": {"name": target},
            "message": {
                "level": "error",
                "code": {"code": code} if code else None,
                "message": message,
            },
        }
    )


class CompareCargoDiagnosticsTest(unittest.TestCase):
    def compare(self, base_lines: list[str], head_lines: list[str]):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory) / "base.jsonl"
            head = Path(directory) / "head.jsonl"
            base.write_text("\n".join(base_lines) + "\n", encoding="utf-8")
            head.write_text("\n".join(head_lines) + "\n", encoding="utf-8")
            return compare.new_errors(base, head)

    def test_same_baseline_error_is_not_new(self):
        error = compiler_message("consumer", "old failure", "E0308")
        self.assertEqual(self.compare([error], [error]), [])

    def test_additional_error_is_detected_when_both_builds_fail(self):
        old = compiler_message("consumer", "old failure", "E0308")
        new = compiler_message("datadog_data_pipeline", "new failure")
        self.assertEqual(
            self.compare([old], [old, new]),
            [
                {
                    "target": "datadog_data_pipeline",
                    "code": "",
                    "message": "new failure",
                }
            ],
        )

    def test_warning_and_non_json_output_are_ignored(self):
        warning = json.dumps(
            {
                "reason": "compiler-message",
                "target": {"name": "consumer"},
                "message": {"level": "warning", "message": "unused"},
            }
        )
        self.assertEqual(self.compare([], ["not json", warning]), [])


if __name__ == "__main__":
    unittest.main()
