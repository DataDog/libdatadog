// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use crate::modes::behavior::Behavior;
use datadog_crashtracker::CrashtrackerConfiguration;

pub struct Test;

impl Behavior for Test {
    fn setup(
        &self,
        _output_dir: &str,
        config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        config.create_alt_stack = false;
        config.use_alt_stack = false;
        Ok(())
    }

    fn pre(&self, _output_dir: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn post(&self, _output_dir: &str) -> anyhow::Result<()> {
        Ok(())
    }
}
