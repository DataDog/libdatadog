// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::arch;
use crate::module::Module;
use crate::utils::{file_replace, project_root};
use anyhow::Result;
use serde::Deserialize;
use std::ffi::OsStr;
use std::fs;
use std::ops::Add;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use tools::headers::dedup_headers;

const CRATE_FOLDER: &str = "datadog-profiling-ffi";

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

        let mut origin_path: PathBuf = [&self.source_include, "dummy.h"].iter().collect();
        let mut target_path: PathBuf = [&self.target_include, "dummy.h"].iter().collect();

        let mut to_dedup = vec![];
        for header in &headers {
            origin_path.set_file_name(header);
            target_path.set_file_name(header);
            fs::copy(&origin_path, &target_path).expect("Failed to copy the header");

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

        fs::copy(from_dyn, to_dyn).expect("unable to copy dynamic lib");

        let from_static: PathBuf = [&self.source_lib, arch::PROF_STATIC_LIB_FFI]
            .iter()
            .collect();
        let to_static: PathBuf = [lib_dir.as_os_str(), OsStr::new(arch::PROF_STATIC_LIB)]
            .iter()
            .collect();

        fs::copy(from_static, to_static).expect("unable to copy static lib");

        arch::add_additional_files(&self.source_lib, lib_dir.as_os_str());

        arch::fix_soname(&self.target_lib);

        // Generate debug information
        arch::strip_libraries(&self.target_lib);
        Ok(())
    }

    fn add_pkg_config(&self) -> Result<()> {
        let files: [&str; 3] = [
            "datadog_profiling.pc",
            "datadog_profiling_with_rpath.pc",
            "datadog_profiling-static.pc",
        ];

        //Create directory
        let pc_dir = Path::new(self.target_pkconfig.as_ref());
        fs::create_dir_all(pc_dir).expect("Failed to create pkgconfig directory");

        // Create files
        for file in files.iter() {
            let file_in = file.to_string() + ".in";

            let mut pc_origin: PathBuf = project_root();
            pc_origin.push(CRATE_FOLDER);
            pc_origin.push(file_in);

            let pc_target: PathBuf = [pc_dir.as_os_str(), OsStr::new(file)].iter().collect();

            file_replace(
                pc_origin.to_str().unwrap(),
                pc_target.to_str().unwrap(),
                "@Datadog_VERSION@",
                &self.version,
            )?;

            if *file == files[2] {
                file_replace(
                    pc_origin.to_str().unwrap(),
                    pc_target.to_str().unwrap(),
                    "@Datadog_LIBRARIES@",
                    arch::NATIVE_LIBS,
                )?;
            }
        }
        Ok(())
    }
}

impl Module for Profiling {
    fn build(&self) -> Result<()> {
        let features = self.features.to_string() + "," + "cbindgen";
        #[cfg(feature = "crashtracker")]
        let features = features.add(",crashtracker-collector,crashtracker-receiver,demangler");

        // Using rustc instead of build in order to overcome issues with LTO optimization.
        let mut cargo_args = vec![
            "rustc",
            "-p",
            CRATE_FOLDER,
            "--features",
            &features,
            "--target",
            &self.arch,
        ];

        if self.profile.as_ref() == "release" {
            cargo_args.push("--release");
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

            let mut cargo = Command::new("cargo")
                .env("RUSTFLAGS", arch::RUSTFLAGS.join(" "))
                .current_dir(project_root())
                .args(args)
                .spawn()
                .expect("failed to spawn cargo");

            cargo.wait().expect("Cargo failed");
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
