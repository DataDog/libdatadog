# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

import importlib.util
import copy
import json
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT_PATH = ROOT / "scripts" / "downstream-impact.py"
SPEC = importlib.util.spec_from_file_location("downstream_impact", SCRIPT_PATH)
assert SPEC and SPEC.loader
downstream_impact = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(downstream_impact)


class DownstreamImpactTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.config = downstream_impact.load_config(
            ROOT / ".github" / "downstream-consumers.json"
        )

    def test_inventory_contains_sixteen_consumers(self):
        self.assertEqual(len(self.config["consumers"]), 16)

    def test_unmapped_documentation_change_has_no_impact(self):
        impact = downstream_impact.calculate_impact(
            self.config, ["docs/RFCs/example.md"]
        )
        self.assertEqual(impact["impacted_count"], 0)
        self.assertEqual(impact["matrix"], {"include": []})

    def test_profiling_change_selects_native_consumers(self):
        impact = downstream_impact.calculate_impact(
            self.config, ["libdd-profiling/src/api.rs"]
        )
        repositories = {item["repository"] for item in impact["impacted"]}
        self.assertIn("DataDog/ddprof", repositories)
        self.assertIn("DataDog/dd-trace-dotnet", repositories)
        self.assertNotIn("DataDog/dd-trace-go", repositories)
        self.assertEqual(impact["validation_count"], 0)

    def test_data_pipeline_change_builds_six_consumers(self):
        impact = downstream_impact.calculate_impact(
            self.config, ["libdd-data-pipeline/src/lib.rs"]
        )
        repositories = {
            item["repository"] for item in impact["validation_matrix"]["include"]
        }
        self.assertEqual(
            repositories,
            {
                "DataDog/dd-trace-rs",
                "DataDog/dd-trace-php",
                "DataDog/dd-trace-py",
                "DataDog/datadog-lambda-extension",
                "DataDog/serverless-components",
                "DataDog/libdatadog-nodejs",
            },
        )
        self.assertEqual(impact["validation_count"], 6)
        by_repository = {
            item["repository"]: item for item in impact["validation_matrix"]["include"]
        }
        self.assertEqual(
            by_repository["DataDog/datadog-lambda-extension"]["patch_sources"],
            ["crates-io", "https://github.com/DataDog/libdatadog"],
        )
        self.assertEqual(
            by_repository["DataDog/dd-trace-php"]["source_path"], "libdatadog"
        )
        self.assertEqual(
            by_repository["DataDog/dd-trace-php"]["submodules"],
            ["appsec/third_party/libddwaf-rust"],
        )
        self.assertEqual(
            by_repository["DataDog/dd-trace-py"]["patch_sources"],
            ["https://github.com/DataDog/libdatadog"],
        )
        self.assertEqual(
            by_repository["DataDog/dd-trace-py"]["build_kind"],
            "python-extension",
        )
        self.assertEqual(
            by_repository["DataDog/libdatadog-nodejs"]["build_kind"],
            "node-wasm",
        )

    def test_workspace_change_selects_every_consumer(self):
        impact = downstream_impact.calculate_impact(self.config, ["Cargo.toml"])
        self.assertEqual(impact["impacted_count"], 16)
        self.assertEqual(impact["validation_count"], 6)

    def test_github_outputs_expose_product_build_matrix(self):
        impact = downstream_impact.calculate_impact(
            self.config, ["libdd-data-pipeline/src/lib.rs"]
        )
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "github-output"
            downstream_impact.write_github_outputs(output, impact)
            values = dict(
                line.split("=", 1)
                for line in output.read_text(encoding="utf-8").splitlines()
            )
        self.assertEqual(values["has_validations"], "true")
        self.assertEqual(values["validation_count"], "6")
        matrix = json.loads(values["validation_matrix"])
        self.assertEqual(len(matrix["include"]), 6)
        self.assertNotIn("cargo_matrix", values)

    def test_less_common_published_component_fails_safe(self):
        impact = downstream_impact.calculate_impact(
            self.config, ["libdd-ffe/src/lib.rs"]
        )
        self.assertEqual(impact["impacted_count"], 16)
        self.assertTrue(
            all(
                item["components"] == ["other-published-components"]
                for item in impact["impacted"]
            )
        )

    def test_components_are_deduplicated(self):
        impact = downstream_impact.calculate_impact(
            self.config,
            [
                "libdd-common/src/lib.rs",
                "libdd-data-pipeline/src/lib.rs",
                "libdd-data-pipeline/src/lib.rs",
            ],
        )
        by_repository = {item["repository"]: item for item in impact["impacted"]}
        self.assertEqual(
            by_repository["DataDog/dd-trace-rs"]["components"],
            ["common-runtime", "data-pipeline"],
        )

    def test_unknown_consumer_is_rejected(self):
        invalid = {
            "schema_version": 1,
            "consumers": [
                {"repository": "DataDog/known", "tier": "direct", "mode": "test"}
            ],
            "components": [
                {"name": "component", "paths": ["src/**"], "consumers": ["DataDog/missing"]}
            ],
        }
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.json"
            path.write_text(json.dumps(invalid), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "unknown consumers"):
                downstream_impact.load_config(path)

    def test_cargo_validation_rejects_two_source_strategies(self):
        invalid = copy.deepcopy(self.config)
        validation = invalid["consumers"][0]["validation"]
        validation["patch_sources"] = ["crates-io"]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.json"
            path.write_text(json.dumps(invalid), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "exactly one"):
                downstream_impact.load_config(path)

    def test_cargo_validation_rejects_source_path_traversal(self):
        invalid = copy.deepcopy(self.config)
        invalid["consumers"][0]["validation"]["source_path"] = "../libdatadog"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.json"
            path.write_text(json.dumps(invalid), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "stay in the checkout"):
                downstream_impact.load_config(path)

    def test_validation_rejects_submodule_path_traversal(self):
        invalid = copy.deepcopy(self.config)
        invalid["consumers"][0]["validation"]["submodules"] = ["../libddwaf-rust"]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.json"
            path.write_text(json.dumps(invalid), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "safe relative paths"):
                downstream_impact.load_config(path)


if __name__ == "__main__":
    unittest.main()
