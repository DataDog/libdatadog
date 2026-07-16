// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Tests that the crashtracker collects stack information for all threads when
//! reporting an unhandled exception (not just signal-based crashes).

use crate::modes::behavior::Behavior;
use libdd_crashtracker::CrashtrackerConfiguration;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

pub struct Test;

impl Behavior for Test {
    fn setup(
        &self,
        _output_dir: &Path,
        config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        config.set_collect_all_threads(true);
        config.set_max_threads(32);
        Ok(())
    }

    fn pre(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn post(&self, _output_dir: &Path) -> anyhow::Result<()> {
        static WORKER_0_READY: AtomicBool = AtomicBool::new(false);
        static WORKER_1_READY: AtomicBool = AtomicBool::new(false);

        #[inline(never)]
        fn worker_fn_0() {
            WORKER_0_READY.store(true, Ordering::Relaxed);
            loop {
                std::hint::black_box(0x21_00u64);
                std::hint::spin_loop();
            }
        }

        #[inline(never)]
        fn worker_fn_1() {
            WORKER_1_READY.store(true, Ordering::Relaxed);
            loop {
                std::hint::black_box(0x21_01u64);
                std::hint::spin_loop();
            }
        }

        WORKER_0_READY.store(false, Ordering::Relaxed);
        WORKER_1_READY.store(false, Ordering::Relaxed);

        let h0 = thread::Builder::new()
            .name("ct_worker_0".to_string())
            .spawn(worker_fn_0)?;

        let h1 = thread::Builder::new()
            .name("ct_worker_1".to_string())
            .spawn(worker_fn_1)?;

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
