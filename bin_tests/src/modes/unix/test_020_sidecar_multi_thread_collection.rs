// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Multi-thread collection test using a sidecar (Unix socket) receiver.
//! Combines the sidecar receiver path (SO_PEERCRED for PR_SET_PTRACER) with
//! collect_all_threads to verify that ptrace works across non-descendant processes.

use crate::modes::behavior::Behavior;
use libdd_crashtracker::CrashtrackerConfiguration;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

pub struct Test;

static WORKER_0_READY: AtomicBool = AtomicBool::new(false);
static WORKER_1_READY: AtomicBool = AtomicBool::new(false);

#[inline(never)]
fn worker_fn_0() {
    WORKER_0_READY.store(true, Ordering::Release);
    loop {
        std::hint::black_box(0x20_00u64);
        std::hint::spin_loop();
    }
}

#[inline(never)]
fn worker_fn_1() {
    WORKER_1_READY.store(true, Ordering::Release);
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

        // Register the expected receiver PID so the crash handler authenticates
        // the socket peer before granting ptrace permission.
        if let Ok(pid_str) = std::env::var("DD_TEST_RECEIVER_PID") {
            if let Ok(pid) = pid_str.parse::<i32>() {
                libdd_crashtracker::set_expected_receiver_pid(pid);
            }
        }

        Ok(())
    }

    fn pre(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn post(&self, _output_dir: &Path) -> anyhow::Result<()> {
        WORKER_0_READY.store(false, Ordering::Release);
        WORKER_1_READY.store(false, Ordering::Release);

        let h0 = thread::Builder::new()
            .name("ct_worker_0".to_string())
            .spawn(worker_fn_0)?;

        let h1 = thread::Builder::new()
            .name("ct_worker_1".to_string())
            .spawn(worker_fn_1)?;

        // Wait until both workers have entered their spin loop function.
        // This eliminates the race where a worker is still in kernel/glibc
        // code (e.g. futex/barrier) when ptrace captures it, which on
        // CentOS 7's glibc 2.17 can produce zero unwind frames.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !WORKER_0_READY.load(Ordering::Acquire) || !WORKER_1_READY.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                panic!("Workers did not reach spin loop within 5s");
            }
            thread::yield_now();
        }

        std::mem::forget(h0);
        std::mem::forget(h1);
        Ok(())
    }
}
