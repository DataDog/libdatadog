// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Maps `builder`'s own Cargo features onto the `libdd-profiling-ffi` feature list used to
//! produce the combined artifact.
//!
//! Data plus a pure function rather than `#[cfg]` attributes, so that combinations no CI job
//! builds can still be asserted in unit tests.

/// What a release build should contain. Selected through `builder`'s Cargo features.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Selection {
    pub telemetry: bool,
    pub data_pipeline: bool,
    pub data_pipeline_compression: bool,
    pub crashtracker: bool,
    pub symbolizer: bool,
    pub library_config: bool,
    pub log: bool,
    pub ddsketch: bool,
    pub ffe: bool,
    pub shared_runtime: bool,
    pub otel_thread_ctx: bool,
    /// Return panics at the FFI boundary as errors instead of aborting the process. A
    /// `builder` default; projects opting out of those defaults must ask for it explicitly.
    pub catch_panic: bool,
}

impl Selection {
    /// Reads the selection from `builder`'s own compiled Cargo features.
    pub fn from_cargo_features() -> Self {
        Self {
            telemetry: cfg!(feature = "telemetry"),
            data_pipeline: cfg!(feature = "data-pipeline"),
            data_pipeline_compression: cfg!(feature = "data-pipeline-compression"),
            crashtracker: cfg!(feature = "crashtracker"),
            symbolizer: cfg!(feature = "symbolizer"),
            library_config: cfg!(feature = "library-config"),
            log: cfg!(feature = "log"),
            ddsketch: cfg!(feature = "ddsketch"),
            ffe: cfg!(feature = "ffe"),
            shared_runtime: cfg!(feature = "shared-runtime"),
            otel_thread_ctx: cfg!(feature = "otel-thread-ctx"),
            catch_panic: cfg!(feature = "catch_panic"),
        }
    }
}

/// Needed whatever else is selected. `ddcommon-ffi` is here because it is a *default* of
/// `libdd-profiling-ffi`, which `--no-default-features` discards.
const ALWAYS: &[&str] = &["cbindgen", "ddcommon-ffi"];

/// The complete `--features` list for `cargo rustc -p libdd-profiling-ffi`.
///
/// Callers **must** also pass `--no-default-features`: the returned list is exhaustive, so
/// nothing is inherited. Inheriting defaults is how `catch_panic` went missing from the
/// shipped artifact in APMSP-3830 without any build failing.
pub fn profiling_features(selection: &Selection) -> Vec<String> {
    let mut features: Vec<&str> = ALWAYS.to_vec();

    if selection.catch_panic {
        features.push("catch_panic");
    }

    if selection.telemetry {
        features.push("ddtelemetry-ffi");
    }
    if selection.data_pipeline {
        features.push("data-pipeline-ffi");
    }
    if selection.data_pipeline_compression {
        features.push("data-pipeline-compression");
    }
    if selection.crashtracker {
        features.extend([
            "crashtracker-ffi",
            "crashtracker-collector",
            "crashtracker-receiver",
            "demangler",
        ]);
    }
    if selection.symbolizer {
        features.push("symbolizer");
    }
    if selection.library_config {
        features.push("datadog-library-config-ffi");
    }
    if selection.log {
        features.push("datadog-log-ffi");
    }
    if selection.ddsketch {
        features.push("ddsketch-ffi");
    }
    if selection.ffe {
        features.push("libdd-ffe-ffi");
    }
    if selection.shared_runtime {
        features.push("shared-runtime");
    }
    if selection.otel_thread_ctx {
        features.push("otel-thread-ctx-ffi");
    }

    features.into_iter().map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use toml::Value;

    /// Every module-selecting flag, so the exhaustive test below cannot silently stop
    /// covering a field that someone adds to [`Selection`].
    const MODULE_SETTERS: &[fn(&mut Selection)] = &[
        |s| s.telemetry = true,
        |s| s.data_pipeline = true,
        |s| s.data_pipeline_compression = true,
        |s| s.crashtracker = true,
        |s| s.symbolizer = true,
        |s| s.library_config = true,
        |s| s.log = true,
        |s| s.ddsketch = true,
        |s| s.ffe = true,
        |s| s.shared_runtime = true,
        |s| s.otel_thread_ctx = true,
    ];

    fn selection_from_bits(bits: u32) -> Selection {
        let mut selection = Selection::default();
        for (index, set) in MODULE_SETTERS.iter().enumerate() {
            if bits & (1 << index) != 0 {
                set(&mut selection);
            }
        }
        selection
    }

    /// APMSP-3830 was the feature being present for some builds and absent for others, so
    /// cover every combination a project can ask for, not just the one we release.
    #[cfg_attr(miri, ignore)] // exhaustive feature combinations are prohibitively slow under Miri
    #[test]
    fn catch_panic_survives_every_module_combination() {
        for bits in 0..(1u32 << MODULE_SETTERS.len()) {
            let mut selection = selection_from_bits(bits);
            selection.catch_panic = true;
            assert!(
                profiling_features(&selection).contains(&"catch_panic".to_string()),
                "catch_panic missing despite being selected, for {selection:?}"
            );
        }
    }

    /// The converse: not asking for it never sneaks it in, whatever else is selected.
    #[cfg_attr(miri, ignore)] // exhaustive feature combinations are prohibitively slow under Miri
    #[test]
    fn catch_panic_is_never_implied_by_another_feature() {
        for bits in 0..(1u32 << MODULE_SETTERS.len()) {
            let selection = selection_from_bits(bits);
            assert!(
                !profiling_features(&selection).contains(&"catch_panic".to_string()),
                "catch_panic emitted without being selected, for {selection:?}"
            );
        }
    }

    /// Exhaustive struct literal on purpose, no `..Default::default()`: adding a field to
    /// [`Selection`] breaks this until it also gets a branch and a `MODULE_SETTERS` entry.
    #[test]
    fn all_fields_set_emits_every_module_feature() {
        let everything = Selection {
            telemetry: true,
            data_pipeline: true,
            data_pipeline_compression: true,
            crashtracker: true,
            symbolizer: true,
            library_config: true,
            log: true,
            ddsketch: true,
            ffe: true,
            shared_runtime: true,
            otel_thread_ctx: true,
            catch_panic: true,
        };

        assert_eq!(
            profiling_features(&everything),
            vec![
                "cbindgen",
                "ddcommon-ffi",
                "catch_panic",
                "ddtelemetry-ffi",
                "data-pipeline-ffi",
                "data-pipeline-compression",
                "crashtracker-ffi",
                "crashtracker-collector",
                "crashtracker-receiver",
                "demangler",
                "symbolizer",
                "datadog-library-config-ffi",
                "datadog-log-ffi",
                "ddsketch-ffi",
                "libdd-ffe-ffi",
                "shared-runtime",
                "otel-thread-ctx-ffi",
            ]
        );
    }

    /// Nothing is inherited under `--no-default-features`, so still-needed defaults must be
    /// listed explicitly.
    #[cfg_attr(miri, ignore)] // exhaustive feature combinations are prohibitively slow under Miri
    #[test]
    fn always_lists_features_that_no_longer_come_from_defaults() {
        for bits in 0..(1u32 << MODULE_SETTERS.len()) {
            let features = profiling_features(&selection_from_bits(bits));
            for required in ALWAYS {
                assert!(
                    features.contains(&required.to_string()),
                    "{required} missing for bits {bits:#b}"
                );
            }
        }
    }

    #[test]
    fn crashtracker_pulls_in_its_sub_features() {
        let selection = Selection {
            crashtracker: true,
            ..Default::default()
        };
        let features = profiling_features(&selection);
        for expected in [
            "crashtracker-ffi",
            "crashtracker-collector",
            "crashtracker-receiver",
            "demangler",
        ] {
            assert!(features.contains(&expected.to_string()), "{expected}");
        }
    }

    #[test]
    fn no_module_selected_emits_no_module_features() {
        let features = profiling_features(&Selection::default());
        assert_eq!(features, vec!["cbindgen", "ddcommon-ffi"]);
    }

    #[cfg_attr(miri, ignore)] // exhaustive feature combinations are prohibitively slow under Miri
    #[test]
    fn emitted_list_has_no_duplicates() {
        for bits in 0..(1u32 << MODULE_SETTERS.len()) {
            let features = profiling_features(&selection_from_bits(bits));
            let mut sorted = features.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), features.len(), "duplicates for {bits:#b}");
        }
    }

    // The tests above only see a hand-built `Selection`. These two cover the ends of the
    // chain: `catch_panic` in builder's `default`, and the fan-out in libdd-profiling-ffi.
    // Neither is reachable from examples/ffi/trace_exporter.c.

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("builder/ has a parent")
            .to_path_buf()
    }

    fn manifest(path: &Path) -> Value {
        fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
            .parse()
            .unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
    }

    fn declares_feature(manifest: &Value, name: &str) -> bool {
        manifest
            .get("features")
            .and_then(Value::as_table)
            .is_some_and(|features| features.contains_key(name))
    }

    /// Entries of a `[features]` list, empty when the feature is absent or has none.
    fn feature<'a>(manifest: &'a Value, name: &str) -> Vec<&'a str> {
        manifest
            .get("features")
            .and_then(Value::as_table)
            .and_then(|features| features.get(name))
            .and_then(Value::as_array)
            .map(|entries| entries.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    }

    #[test]
    fn builder_defaults_to_panic_containment() {
        let builder = manifest(&workspace_root().join("builder/Cargo.toml"));

        assert!(
            feature(&builder, "default").contains(&"catch_panic"),
            "builder's default must include catch_panic, or the artifact aborts the host \
             process on a Rust panic"
        );
    }

    #[cfg_attr(miri, ignore)] // exhaustive feature combinations are prohibitively slow under Miri
    #[test]
    fn every_containment_capable_sub_crate_is_fanned_out() {
        let root = workspace_root();
        let aggregator_dir = root.join("libdd-profiling-ffi");
        let aggregator = manifest(&aggregator_dir.join("Cargo.toml"));
        let fan_out = feature(&aggregator, "catch_panic");

        let dependencies = aggregator
            .get("dependencies")
            .and_then(Value::as_table)
            .expect("libdd-profiling-ffi declares [dependencies]");

        let mut found = 0;
        for (name, spec) in dependencies {
            if spec.get("optional").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            let Some(path) = spec.get("path").and_then(Value::as_str) else {
                continue;
            };
            let sub_crate = manifest(&aggregator_dir.join(path).join("Cargo.toml"));
            if !declares_feature(&sub_crate, "catch_panic") {
                continue;
            }

            found += 1;
            let entry = format!("{name}?/catch_panic");
            assert!(
                fan_out.contains(&entry.as_str()),
                "{name} has a catch_panic feature that libdd-profiling-ffi's catch_panic does \
                 not enable; add \"{entry}\""
            );
        }

        assert!(
            found > 0,
            "no containment-capable optional dependency found, so this test is no longer \
             checking anything"
        );
    }
}
