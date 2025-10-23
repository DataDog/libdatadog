// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::arch;
use crate::module::Module;
use crate::utils::project_root;
use anyhow::Result;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;

pub struct Common {
    pub arch: Rc<str>,
    pub source_include: Rc<str>,
    pub target_include: Rc<str>,
}

impl Module for Common {
    fn build(&self) -> Result<()> {
        let mut cargo = Command::new("cargo")
            .env("RUSTFLAGS", arch::RUSTFLAGS.join(" "))
            .current_dir(project_root())
            .args([
                "build",
                "-p",
                "ddcommon-ffi",
                "--features",
                "cbindgen",
                "--target",
                &self.arch,
            ])
            .spawn()
            .expect("failed to spawn cargo");

        cargo.wait().expect("Cargo failed");
        Ok(())
    }

    fn install(&self) -> Result<()> {
        let target_path: PathBuf = [self.target_include.as_ref(), "common.h"].iter().collect();

        let origin_path: PathBuf = [self.source_include.as_ref(), "common.h"].iter().collect();
        fs::copy(origin_path, target_path).expect("Failed to copy common.h");
        Ok(())
    }
}
