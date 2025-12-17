// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// Exercise the collector under an LD_PRELOAD malloc logger.

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
        use anyhow::Context;

        // Ensure the collector loads the malloc logger via LD_PRELOAD.
        if std::env::var("LD_PRELOAD").is_err() {
            let so_path = option_env!("MALLOC_LOGGER_SO")
                .context("MALLOC_LOGGER_SO not set; rebuild bin_tests?")?;
            std::env::set_var("LD_PRELOAD", so_path);
        }

        // Direct malloc log into the test output directory.
        let log_path = output_dir.join("malloc_logger.log");
        std::env::set_var("MALLOC_LOG_PATH", &log_path);
        // Enable logging now that path is set.
        std::env::set_var("MALLOC_LOG_ENABLED", "1");
        let _ = std::fs::remove_file(&log_path);
        Ok(())
    }

    fn pre(&self, _output_dir: &Path) -> anyhow::Result<()> {
        // The collector (this process) should already be running with LD_PRELOAD.
        // Drop LD_PRELOAD before spawning the receiver to keep the preload scoped
        // to the collector only.
        std::env::remove_var("LD_PRELOAD");
        Ok(())
    }

    fn post(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }
}
