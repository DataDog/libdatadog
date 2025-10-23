// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use crate::modes::behavior::Behavior;
use libdd_crashtracker::CrashtrackerConfiguration;
use std::path::Path;
// This is a simple baseline test that ensures the crashtracker is capable of running on the normal
// stack during a signal (e.g., not using the sigaltstack).  Rather than setting any complicated
// state, it just mutates the configuration to disable the creation and use of the altstack. If the
// crashtracker is working, then it will still be able to produce and handle a crash as normal.

pub struct Test;

impl Behavior for Test {
    fn setup(
        &self,
        _output_dir: &Path,
        config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        config.set_create_alt_stack(false)?;
        config.set_use_alt_stack(false)?;
        Ok(())
    }

    fn pre(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn post(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }
}
