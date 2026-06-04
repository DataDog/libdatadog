// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Multi-thread collection test using a sidecar (Unix socket) receiver.
//! Combines the sidecar receiver path (SO_PEERCRED for PR_SET_PTRACER) with
//! collect_all_threads to verify that ptrace works across non-descendant processes.

use crate::modes::behavior::Behavior;
use libdd_crashtracker::CrashtrackerConfiguration;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

pub struct Test;

#[inline(never)]
fn worker_fn_0() {
    loop {
        std::hint::black_box(0x20_00u64);
        std::hint::spin_loop();
    }
}

#[inline(never)]
fn worker_fn_1() {
    loop {
        std::hint::black_box(0x20_01u64);
        std::hint::spin_loop();
    }
}

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
        config.set_max_threads(32);
        Ok(())
    }

    fn pre(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn post(&self, _output_dir: &Path) -> anyhow::Result<()> {
        let barrier = Arc::new(Barrier::new(3));

        let b0 = Arc::clone(&barrier);
        let h0 = thread::Builder::new()
            .name("ct_worker_0".to_string())
            .spawn(move || {
                b0.wait();
                worker_fn_0();
            })?;

        let b1 = Arc::clone(&barrier);
        let h1 = thread::Builder::new()
            .name("ct_worker_1".to_string())
            .spawn(move || {
                b1.wait();
                worker_fn_1();
            })?;

        barrier.wait();
        thread::sleep(Duration::from_millis(20));

        std::mem::forget(h0);
        std::mem::forget(h1);
        Ok(())
    }
}
