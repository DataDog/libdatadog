// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Stress-tests thread collection by spawning 512 named worker threads and
//! verifying the crash report contains all of them.

use crate::modes::behavior::Behavior;
use libdd_crashtracker::{default_max_threads, CrashtrackerConfiguration};
use std::path::Path;
use std::sync::{Arc, Barrier};

pub struct Test;

fn worker_fn(i: usize) {
    std::hint::black_box(i);
    std::thread::sleep(std::time::Duration::from_millis(1000))
}

const THREAD_COUNT: usize = default_max_threads();

impl Behavior for Test {
    fn setup(
        &self,
        _output_dir: &Path,
        config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        config.set_collect_all_threads(true);
        config.set_max_threads(THREAD_COUNT);
        Ok(())
    }

    fn pre(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn post(&self, _output_dir: &Path) -> anyhow::Result<()> {
        let barrier = Arc::new(Barrier::new(THREAD_COUNT + 1));

        let _: Vec<_> = (0..THREAD_COUNT)
            .map(|i| {
                let barrier = Arc::clone(&barrier);
                std::thread::Builder::new()
                    .name(format!("worker-{i}"))
                    .spawn(move || {
                        barrier.wait();
                        worker_fn(i);
                    })
                    .expect("failed to spawn thread")
            })
            .collect();

        barrier.wait();
        Ok(())
    }
}
