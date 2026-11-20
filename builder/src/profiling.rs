// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::arch;
use crate::module::Module;
use crate::utils::{adjust_extern_symbols, project_root, wait_for_success};
use anyhow::{Result, bail};
use serde::Deserialize;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use tools::headers::dedup_headers;

const CRATE_FOLDER: &str = "libdd-profiling-ffi";

/// `no_mangle` static defined by `libdd-profiling` only under `allocation_chaos_test`.
/// Must match `ALLOC_CHAOS_OBSERVER` in `libdd-profiling/src/arena.rs`.
const CHAOS_MARKER: &[u8] = b"ALLOC_CHAOS_OBSERVER";

/// The release `cargo rustc` is a separate invocation from the allocation-failure harness.
/// Chaos is a cfg (`LIBDD_ALLOCATION_CHAOS_TEST=1`), not a Cargo feature, so feature
/// unification cannot enable it here. Refuse the env and the old feature name anyway:
/// a distributable artifact must not be a chaos build.
fn reject_allocation_chaos(features: &str, chaos_env: Option<&OsStr>) -> Result<()> {
    if chaos_env.is_some() {
        bail!(
            "refusing to build distributable profiling artifacts: LIBDD_ALLOCATION_CHAOS_TEST is set. \
             The allocation-failure harness is a separate Cargo invocation"
        );
    }
    if features
        .split(',')
        .any(|feature| feature.trim() == "allocation-chaos-test")
    {
        bail!(
            "refusing to build distributable profiling artifacts: allocation-chaos-test is test-only"
        );
    }
    Ok(())
}

fn contains_chaos_marker(bytes: &[u8]) -> bool {
    bytes
        .windows(CHAOS_MARKER.len())
        .any(|window| window == CHAOS_MARKER)
}

fn assert_no_chaos_marker(path: &Path) -> Result<()> {
    let bytes = fs::read(path).map_err(|err| anyhow::anyhow!("read {}: {err}", path.display()))?;
    if contains_chaos_marker(&bytes) {
        bail!(
            "released library {} contains the allocation-chaos marker ALLOC_CHAOS_OBSERVER",
            path.display()
        );
    }
    Ok(())
}

#[derive(Deserialize)]
struct CargoFile {
    lib: LibSection,
}

#[derive(Deserialize)]
struct LibSection {
    #[serde(rename = "crate-type")]
    crate_type: Vec<String>,
}

pub struct Profiling {
    pub arch: Rc<str>,
    pub base_header: Rc<str>,
    pub features: Rc<str>,
    pub profile: Rc<str>,
    pub source_include: Rc<str>,
    pub source_lib: Rc<str>,
    pub target_include: Rc<str>,
    pub target_lib: Rc<str>,
    pub target_pkconfig: Rc<str>,
    pub version: Rc<str>,
    /// Overrides the `FileVersion` embedded in the Windows artifacts' `VERSIONINFO`
    /// resource. Forwarded to the build script through `LIBDATADOG_PACKAGE_VERSION`;
    /// `None` leaves it at the libdatadog release version.
    pub file_version: Option<Rc<str>>,
}

impl Profiling {
    fn add_headers(&self) -> Result<()> {
        // Allowing unused_mut due to the methods mutating the vector are behind a feature flag.
        #[allow(unused_mut)]
        let mut headers = vec!["profiling.h"];
        #[cfg(feature = "telemetry")]
        headers.push("telemetry.h");
        #[cfg(feature = "data-pipeline")]
        headers.push("data-pipeline.h");
        #[cfg(feature = "symbolizer")]
        headers.push("blazesym.h");
        #[cfg(feature = "library-config")]
        headers.push("library-config.h");
        #[cfg(feature = "log")]
        headers.push("log.h");
        #[cfg(feature = "ddsketch")]
        headers.push("ddsketch.h");
        #[cfg(feature = "ffe")]
        headers.push("ffe.h");
        #[cfg(feature = "shared-runtime")]
        headers.push("shared-runtime.h");
        #[cfg(feature = "otel-thread-ctx")]
        headers.push("otel-thread-ctx.h");

        let mut origin_path: PathBuf = [&self.source_include, "dummy.h"].iter().collect();
        let mut target_path: PathBuf = [&self.target_include, "dummy.h"].iter().collect();

        let mut to_dedup = vec![];
        for header in &headers {
            origin_path.set_file_name(header);
            target_path.set_file_name(header);
            adjust_extern_symbols(&origin_path, &target_path)
                .expect("Failed to adjust extern symbols");

            // Exclude blazesym header from deduplication
            if !target_path.to_str().unwrap().contains("blazesym.h") {
                to_dedup.push(target_path.clone());
            }
        }

        dedup_headers(
            self.base_header.as_ref(),
            &(to_dedup
                .iter()
                .map(|i| i.to_str().unwrap())
                .collect::<Vec<&str>>()),
        );

        Ok(())
    }

    fn add_libs(&self) -> Result<()> {
        //Create directory
        let lib_dir = Path::new(self.target_lib.as_ref());
        fs::create_dir_all(lib_dir).expect("Failed to create pkgconfig directory");

        let from_dyn: PathBuf = [&self.source_lib, arch::PROF_DYNAMIC_LIB_FFI]
            .iter()
            .collect();
        let to_dyn: PathBuf = [lib_dir.as_os_str(), OsStr::new(arch::PROF_DYNAMIC_LIB)]
            .iter()
            .collect();

        fs::copy(from_dyn, &to_dyn).expect("unable to copy dynamic lib");

        let from_static: PathBuf = [&self.source_lib, arch::PROF_STATIC_LIB_FFI]
            .iter()
            .collect();
        let to_static: PathBuf = [lib_dir.as_os_str(), OsStr::new(arch::PROF_STATIC_LIB)]
            .iter()
            .collect();

        fs::copy(from_static, &to_static).expect("unable to copy static lib");

        // Scan the bytes that will be shipped. Strip keeps the symbol name, and a chaos
        // build always contains this marker, so its absence means the original allocator.
        assert_no_chaos_marker(&to_dyn)?;
        assert_no_chaos_marker(&to_static)?;

        arch::add_additional_files(&self.source_lib, lib_dir.as_os_str());

        // Generate debug information
        arch::strip_libraries(&self.target_lib);
        Ok(())
    }

    fn add_pkg_config(&self) -> Result<()> {
        arch::add_pkg_config(
            CRATE_FOLDER,
            self.target_pkconfig.as_ref(),
            self.version.as_ref(),
        )
    }
}

impl Module for Profiling {
    fn build(&self) -> Result<()> {
        reject_allocation_chaos(
            &self.features,
            std::env::var_os("LIBDD_ALLOCATION_CHAOS_TEST").as_deref(),
        )?;

        // Using rustc instead of build in order to overcome issues with LTO optimization.
        // This invocation is not the harness (`cargo test -p libdd-profiling-ffi --test
        // allocation_failure`), so a chaos cfg enabled for the harness is not unified in.
        let mut cargo_args = vec![
            "rustc",
            "-p",
            CRATE_FOLDER,
            "--no-default-features",
            "--features",
            &self.features,
            "--target",
            &self.arch,
        ];

        match self.profile.as_ref() {
            "debug" | "dev" => {}
            profile => {
                cargo_args.push("--profile");
                cargo_args.push(profile);
            }
        }

        // Parse profiling-ffi manifest in order to get the crate-type array.
        let prof_path: PathBuf = [project_root().to_str().unwrap(), CRATE_FOLDER, "Cargo.toml"]
            .iter()
            .collect();
        // Buffer the manifest file.
        let cargo_toml = fs::read_to_string(prof_path).unwrap();
        // Use serde to get access to the lib section.
        let parsed: CargoFile = toml::from_str(&cargo_toml).unwrap();
        // Iterate over all the crate types in order to build the artifacts for each one.
        for crate_type in parsed.lib.crate_type.iter() {
            // Ignore lib crate-type
            if crate_type == "lib" {
                continue;
            }

            let mut args = cargo_args.clone();
            args.append(&mut vec!["--crate-type", crate_type]);

            let mut cargo = Command::new("cargo");
            cargo
                .env("RUSTFLAGS", arch::RUSTFLAGS.join(" "))
                .env_remove("LIBDD_ALLOCATION_CHAOS_TEST")
                .current_dir(project_root())
                .args(args);
            if let Some(file_version) = &self.file_version {
                cargo.env("LIBDATADOG_PACKAGE_VERSION", file_version.as_ref());
            }
            let cargo = cargo.spawn().expect("failed to spawn cargo");

            wait_for_success(cargo, "Cargo");
        }

        Ok(())
    }
    fn install(&self) -> Result<()> {
        self.add_headers()?;
        self.add_libs()?;
        self.add_pkg_config()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{contains_chaos_marker, reject_allocation_chaos};
    use std::ffi::OsStr;

    #[test]
    fn release_build_rejects_the_chaos_env() {
        let err = reject_allocation_chaos("cbindgen,ddcommon-ffi", Some(OsStr::new("1")))
            .expect_err("chaos env");
        assert!(err.to_string().contains("LIBDD_ALLOCATION_CHAOS_TEST"));
    }

    #[test]
    fn release_build_rejects_the_chaos_feature() {
        let err =
            reject_allocation_chaos("cbindgen,allocation-chaos-test", None).expect_err("feature");
        assert!(err.to_string().contains("allocation-chaos-test"));
    }

    #[test]
    fn release_build_allows_a_normal_feature_list() {
        reject_allocation_chaos("cbindgen,ddcommon-ffi", None).expect("normal features");
    }

    #[test]
    fn chaos_marker_scan_matches_the_symbol_name() {
        assert!(!contains_chaos_marker(b""));
        assert!(!contains_chaos_marker(b"ALLOC_CHAOS"));
        assert!(contains_chaos_marker(b"xxALLOC_CHAOS_OBSERVERyy"));
    }
}
