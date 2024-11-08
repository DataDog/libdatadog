// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::arch;
use crate::module::Module;
use anyhow::Result;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

pub struct CrashTracker {
    pub source_include: Rc<str>,
    pub target_dir: Rc<str>,
    pub target_include: Rc<str>,
}

impl CrashTracker {
    fn add_binaries(&self) -> Result<()> {
        let _dst = cmake::Config::new("../crashtracker")
            .define("Datadog_ROOT", self.target_dir.as_ref())
            .define("CMAKE_INSTALL_PREFIX", self.target_dir.as_ref())
            .build();

        Ok(())
    }

    fn add_headers(&self) -> Result<()> {
        let origin_path: PathBuf = [self.source_include.as_ref(), "crashtracker.h"]
            .iter()
            .collect();
        let target_path: PathBuf = [self.target_include.as_ref(), "crashtracker.h"]
            .iter()
            .collect();
        fs::copy(origin_path, target_path).expect("Failed to copy crashtracker.h");

        Ok(())
    }
}

impl Module for CrashTracker {
    fn install(&self) -> Result<()> {
        self.add_headers()?;
        if arch::BUILD_CRASHTRACKER {
            self.add_binaries()?;
        }
        Ok(())
    }
}
