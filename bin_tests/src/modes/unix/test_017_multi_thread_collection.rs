// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Tests that the crashtracker collects stack information for all threads, not
//! just the crashing thread.

use crate::modes::behavior::Behavior;
use libdd_crashtracker::CrashtrackerConfiguration;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

pub struct Test;

// Black box to prevent compiler optim
#[inline(never)]
fn worker_fn_0() {
    loop {
        std::hint::black_box(0x17_00u64); // 0x17 = test 017; _00 = thread 0
        std::hint::spin_loop();
    }
}

#[inline(never)]
fn worker_fn_1() {
    loop {
        std::hint::black_box(0x17_01u64); // 0x17 = test 017; _01 = thread 1
        std::hint::spin_loop();
    }
}

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
