// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::arch;
use crate::module::Module;
use anyhow::{anyhow, Result};
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;

fn file_replace(file_in: &str, file_out: &str, target: &str, replace: &str) -> Result<()> {
    let content = fs::read_to_string(file_in)?;
    let new = content.replace(target, replace);

    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .create(true)
        .open(file_out)?;
    file.write_all(new.as_bytes())
        .map_err(|err| anyhow!("failed to write file: {}", err))
}

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
        let pc_dir = Path::new(self.target_pkconfig.as_ref());
        fs::create_dir_all(pc_dir).expect("Failed to create pkgconfig directory");

        // Create files
        for file in files.iter() {
            let file_in = "../profiling-ffi/".to_string() + file + ".in";

            let pc_file = pc_dir.to_str().unwrap().to_string() + "/" + *file;

            file_replace(&file_in, &pc_file, "@Datadog_VERSION@", &self.version)?;

            if *file == files[2] {
                file_replace(&file_in, &pc_file, "@Datadog_LIBRARIES@", arch::NATIVE_LIBS)?;
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
