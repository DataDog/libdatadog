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
    def compare(
        self,
        base_lines: list[str],
        head_lines: list[str],
        base_log: str = "",
        head_log: str = "",
        base_normalize_paths: list[Path] | None = None,
        head_normalize_paths: list[Path] | None = None,
    ):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory) / "base.jsonl"
            head = Path(directory) / "head.jsonl"
            base.write_text("\n".join(base_lines) + "\n", encoding="utf-8")
            head.write_text("\n".join(head_lines) + "\n", encoding="utf-8")
            base_log_path = Path(directory) / "base.log"
            head_log_path = Path(directory) / "head.log"
            base_log_path.write_text(base_log, encoding="utf-8")
            head_log_path.write_text(head_log, encoding="utf-8")
            return compare.new_errors(
                base,
                head,
                base_log_path,
                head_log_path,
                base_normalize_paths,
                head_normalize_paths,
            )

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

    def test_new_cargo_resolution_error_is_detected(self):
        errors = self.compare(
            [],
            [],
            base_log="error: failed to select a version for `old-dependency`\n",
            head_log="error: invalid table header\n",
        )
        self.assertEqual(
            errors,
            [{"target": "cargo", "code": "", "message": "invalid table header"}],
        )

    def test_text_log_is_ignored_when_compiler_diagnostics_exist(self):
        compiler_error = compiler_message("consumer", "new compiler error", "E0425")
        errors = self.compare(
            [],
            [compiler_error],
            head_log="error: could not compile `consumer`\n",
        )
        self.assertEqual(len(errors), 1)
        self.assertEqual(errors[0]["message"], "new compiler error")

    def test_candidate_specific_paths_do_not_look_like_new_errors(self):
        base_root = Path("/work/libdatadog-base")
        head_root = Path("/work/libdatadog-head")
        errors = self.compare(
            [],
            [],
            base_log=f"error: failed to load {base_root}/libdd-common/Cargo.toml\n",
            head_log=f"error: failed to load {head_root}/libdd-common/Cargo.toml\n",
            base_normalize_paths=[base_root],
            head_normalize_paths=[head_root],
        )
        self.assertEqual(errors, [])


if __name__ == "__main__":
    unittest.main()
