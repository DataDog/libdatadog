# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

import importlib.util
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT_PATH = ROOT / "scripts" / "select-locked-libdatadog-packages.py"
SPEC = importlib.util.spec_from_file_location(
    "select_locked_libdatadog_packages", SCRIPT_PATH
)
assert SPEC and SPEC.loader
selector = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(selector)


class SelectLockedLibdatadogPackagesTest(unittest.TestCase):
    def write_lockfile(self, directory: str, packages: str) -> Path:
        path = Path(directory) / "Cargo.lock"
        path.write_text(f'version = 3\n{packages}', encoding="utf-8")
        return path

    def test_selects_only_matching_libdatadog_crates_from_crates_io(self):
        with tempfile.TemporaryDirectory() as directory:
            lockfile = self.write_lockfile(
                directory,
                '''
[[package]]
name = "datadog-trace-utils"
version = "0.7.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "datadog-trace-utils"
version = "0.8.0"
source = "git+https://github.com/DataDog/libdatadog?rev=abc#123"
''',
            )
            self.assertEqual(
                selector.select_package_specs(
                    lockfile,
                    {"datadog-trace-utils"},
                    ["crates-io"],
                ),
                ["datadog-trace-utils@0.7.0"],
            )

    def test_matches_git_sources_across_query_and_dot_git_variants(self):
        with tempfile.TemporaryDirectory() as directory:
            lockfile = self.write_lockfile(
                directory,
                '''
[[package]]
name = "datadog-data-pipeline"
version = "0.1.0"
source = "git+https://github.com/DataDog/libdatadog?rev=main#123"
''',
            )
            self.assertEqual(
                selector.select_package_specs(
                    lockfile,
                    {"datadog-data-pipeline"},
                    ["https://github.com/DataDog/libdatadog.git"],
                ),
                ["datadog-data-pipeline@0.1.0"],
            )

    def test_ignores_path_packages_and_other_git_sources(self):
        with tempfile.TemporaryDirectory() as directory:
            lockfile = self.write_lockfile(
                directory,
                '''
[[package]]
name = "datadog-common"
version = "0.1.0"

[[package]]
name = "datadog-data-pipeline"
version = "0.1.0"
source = "git+https://github.com/example/libdatadog#123"
''',
            )
            self.assertEqual(
                selector.select_package_specs(
                    lockfile,
                    {"datadog-common", "datadog-data-pipeline"},
                    ["https://github.com/DataDog/libdatadog"],
                ),
                [],
            )

    def test_crates_io_patch_ignores_other_registries(self):
        with tempfile.TemporaryDirectory() as directory:
            lockfile = self.write_lockfile(
                directory,
                '''
[[package]]
name = "datadog-common"
version = "0.1.0"
source = "registry+https://example.com/cargo-index"
''',
            )
            self.assertEqual(
                selector.select_package_specs(
                    lockfile,
                    {"datadog-common"},
                    ["crates-io"],
                ),
                [],
            )

    def test_missing_lockfile_needs_no_update(self):
        with tempfile.TemporaryDirectory() as directory:
            self.assertEqual(
                selector.select_package_specs(
                    Path(directory) / "Cargo.lock",
                    {"datadog-common"},
                    ["crates-io"],
                ),
                [],
            )

    def test_rejects_ambiguous_name_and_version(self):
        with tempfile.TemporaryDirectory() as directory:
            lockfile = self.write_lockfile(
                directory,
                '''
[[package]]
name = "datadog-common"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "datadog-common"
version = "0.1.0"
source = "git+https://github.com/DataDog/libdatadog#123"
''',
            )
            with self.assertRaisesRegex(ValueError, "ambiguous locked package"):
                selector.select_package_specs(
                    lockfile,
                    {"datadog-common"},
                    ["crates-io", "https://github.com/DataDog/libdatadog"],
                )


if __name__ == "__main__":
    unittest.main()
