// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::arch;
use crate::module::Module;
use crate::utils::project_root;
use anyhow::Result;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use tools::headers::dedup_headers;

pub struct CrashTracker {
    pub arch: Rc<str>,
    pub base_header: Rc<str>,
    pub profile: Rc<str>,
    pub source_include: Rc<str>,
    pub target_dir: Rc<str>,
    pub target_include: Rc<str>,
}

impl CrashTracker {
    fn gen_binaries(&self) -> Result<()> {
        if arch::BUILD_CRASHTRACKER {
            let mut crashtracker_dir = project_root();
            crashtracker_dir.push("crashtracker");
            let _dst = cmake::Config::new(crashtracker_dir.to_str().unwrap())
                .define("Datadog_ROOT", self.target_dir.as_ref())
                .define("CMAKE_INSTALL_PREFIX", self.target_dir.as_ref())
                .build();
        }

        Ok(())
    }
    fn add_headers(&self) -> Result<()> {
        let origin_path: PathBuf = [self.source_include.as_ref(), "crashtracker.h"]
            .iter()
            .collect();
        let target_path: PathBuf = [self.target_include.as_ref(), "crashtracker.h"]
            .iter()
            .collect();

        let headers = vec![target_path.to_str().unwrap()];
        fs::copy(origin_path, &target_path).expect("Failed to copy crashtracker.h");

        dedup_headers(self.base_header.as_ref(), &headers);

        Ok(())
    }
}

impl Module for CrashTracker {
    fn build(&self) -> Result<()> {
        Ok(())
    }

    fn install(&self) -> Result<()> {
        self.add_headers()?;
        self.gen_binaries()?;
        Ok(())
    }
}
