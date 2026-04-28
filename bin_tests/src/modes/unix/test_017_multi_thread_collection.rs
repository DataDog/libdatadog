// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Tests that the crashtracker collects stack information for all threads, not
//! just the crashing thread.
//!
//! Two named background threads are spawned with distinct, recognisable call
//! chains so that the captured stacks are visually interesting and clearly
//! distinguishable in the crash report.
//!
//! ct_worker_0: worker_entry_0 -> wait_for_work_0 -> spin_loop
//! ct_worker_1: worker_entry_1 -> wait_for_work_1 -> spin_loop
//!
//! All intermediate functions are #[inline(never)] so they appear as distinct
//! frames in the libunwind output.

use crate::modes::behavior::Behavior;
use libdd_crashtracker::CrashtrackerConfiguration;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

pub struct Test;

#[inline(never)]
fn wait_for_work_0() {
    // Distinct black_box constant prevents the linker's identical-code-folding
    // pass from merging wait_for_work_0 and wait_for_work_1 into one symbol.
    let _ = std::hint::black_box(10u64);
    loop {
        std::hint::spin_loop();
    }
}

#[inline(never)]
fn worker_entry_0() {
    let _ = std::hint::black_box(20u64);
    wait_for_work_0();
}

#[inline(never)]
fn wait_for_work_1() {
    let _ = std::hint::black_box(11u64);
    loop {
        std::hint::spin_loop();
    }
}

#[inline(never)]
fn worker_entry_1() {
    let _ = std::hint::black_box(21u64);
    wait_for_work_1();
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

    /// Spawn two named worker threads with distinct call chains, then leak the
    /// handles so the threads outlive post() and are still live when the crash fires.
    fn post(&self, _output_dir: &Path) -> anyhow::Result<()> {
        // 2 workers + 1 for this (main) thread.
        let barrier = Arc::new(Barrier::new(3));

        let b0 = Arc::clone(&barrier);
        let h0 = thread::Builder::new()
            .name("ct_worker_0".to_string())
            .spawn(move || {
                b0.wait();
                worker_entry_0();
            })?;

        let b1 = Arc::clone(&barrier);
        let h1 = thread::Builder::new()
            .name("ct_worker_1".to_string())
            .spawn(move || {
                b1.wait();
                worker_entry_1();
            })?;

        barrier.wait();
        thread::sleep(Duration::from_millis(20));

        std::mem::forget(h0);
        std::mem::forget(h1);
        Ok(())
    }
}
