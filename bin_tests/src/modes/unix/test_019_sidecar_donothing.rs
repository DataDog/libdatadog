// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Baseline crash test using a sidecar (Unix socket) receiver instead of fork/exec.
//! The socket path is passed using the DD_TEST_UNIX_SOCKET_PATH env var, which
//! the integration test sets after spawning the receiver process.

use crate::modes::behavior::Behavior;
use libdd_crashtracker::CrashtrackerConfiguration;
use std::path::Path;

pub struct Test;

impl Behavior for Test {
    fn setup(
        &self,
        _output_dir: &Path,
        config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        let socket_path = std::env::var("DD_TEST_UNIX_SOCKET_PATH")
            .expect("DD_TEST_UNIX_SOCKET_PATH must be set for sidecar tests");
        config.set_unix_socket_path(socket_path);
        Ok(())
    }

    fn pre(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn post(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }
}
