// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::module::Module;
use anyhow::Result;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

pub struct Common {
    pub source_include: Rc<str>,
    pub target_include: Rc<str>,
}

impl Module for Common {
    fn install(&self) -> Result<()> {
        let target_path: PathBuf = [self.target_include.as_ref(), "common.h"].iter().collect();

        let origin_path: PathBuf = [self.source_include.as_ref(), "common.h"].iter().collect();
        fs::copy(origin_path, target_path).expect("Failed to copy common.h");
        Ok(())
    }
}