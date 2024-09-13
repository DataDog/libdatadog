// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::arch;
use crate::module::Module;
use anyhow::Result;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

pub struct Profiling {
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

        let mut origin_path: PathBuf = [&self.source_include, "dummy.h"].iter().collect();
        let mut target_path: PathBuf = [&self.target_include, "dummy.h"].iter().collect();

        for header in headers {
            origin_path.set_file_name(header);
            target_path.set_file_name(header);
            fs::copy(&origin_path, &target_path).expect("Failed to copy the header");
        }

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
        // let pc_dir: PathBuf = [&self, "lib/pkgconfig"].iter().collect();
        let pc_dir = Path::new(self.target_pkconfig.as_ref());
        fs::create_dir_all(pc_dir).expect("Failed to create pkgconfig directory");

        // Create files
        for file in files.iter() {
            let file_in = "../profiling-ffi/".to_string() + file + ".in";
            let output = Command::new("sed")
                .arg("s/@Datadog_VERSION@/".to_string() + &self.version + "/g")
                .arg(&file_in)
                .output()
                .expect("sed command failed");

            let pc_file: PathBuf = [pc_dir.as_os_str(), OsStr::new(file)].iter().collect();
            fs::write(&pc_file, &output.stdout).expect("writing pc file failed");

            if *file == files[2] {
                let output = Command::new("sed")
                    .arg("s/@Datadog_LIBRARIES@/".to_string() + arch::NATIVE_LIBS + "/g")
                    .arg(&file_in)
                    .output()
                    .expect("sed command failed");

                fs::write(&pc_file, &output.stdout).expect("writing pc file failed");
            }
        }
        Ok(())
    }
}

impl Module for Profiling {
    fn install(&self) -> Result<()> {
        self.add_headers()?;
        self.add_libs()?;
        self.add_pkg_config()?;
        Ok(())
    }
}
