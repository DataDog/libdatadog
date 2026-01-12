// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// Exercise the collector under an LD_PRELOAD preload logger.

use crate::modes::behavior::Behavior;
use libdd_crashtracker::CrashtrackerConfiguration;
use std::path::Path;

pub struct Test;

impl Behavior for Test {
    fn setup(
        &self,
        output_dir: &Path,
        _config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        let log_path = output_dir.join("preload_logger.log");
        std::env::set_var("PRELOAD_LOG_PATH", &log_path);
        let _ = std::fs::remove_file(&log_path);
        Ok(())
    }

    fn pre(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn post(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }
}
