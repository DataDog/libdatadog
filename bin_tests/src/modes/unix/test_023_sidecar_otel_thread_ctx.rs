// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTel thread context collection via a sidecar (Unix socket) receiver.
//! The collector reads the crashing thread's OTel record from its own TLS
//! and ptraces the other threads.

use crate::modes::behavior::Behavior;
use libdd_crashtracker::CrashtrackerConfiguration;
use std::path::Path;

pub use super::test_022_otel_thread_ctx::{MAIN_SPAN_ID, MAIN_TRACE_ID};

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
        config.set_collect_all_threads(true);
        config.set_collect_otel_thread_contexts(true);
        config.set_max_threads(32);

        if let Ok(pid_str) = std::env::var("DD_TEST_RECEIVER_PID")
            && let Ok(pid) = pid_str.parse::<i32>()
        {
            libdd_crashtracker::set_expected_receiver_pid(pid);
        }

        Ok(())
    }

    fn pre(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn post(&self, output_dir: &Path) -> anyhow::Result<()> {
        super::test_022_otel_thread_ctx::Test.post(output_dir)
    }
}
