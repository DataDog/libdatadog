// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::{
    future::{BoxFuture, Shared},
    FutureExt,
};

use tokio::{select, sync::mpsc::Receiver};

pub struct Watchdog {
    interval: tokio::time::Interval,
    max_memory_usage_bytes: usize,
    shutdown_receiver: Receiver<()>,
}

#[derive(Clone)]
pub struct WatchdogHandle {
    handle: Shared<BoxFuture<'static, Option<()>>>,
    pub mem_usage_bytes: Arc<AtomicUsize>,
}

impl WatchdogHandle {
    pub async fn wait_for_shutdown(&self) {
        self.handle.clone().await;
    }
}

impl Watchdog {
    pub fn from_receiver(shutdown_receiver: Receiver<()>) -> Self {
        Watchdog {
            interval: tokio::time::interval(Duration::from_secs(60)),
            max_memory_usage_bytes: 1024 * 1024 * 1024, // 1 GB
            shutdown_receiver,
        }
    }

    pub fn spawn_watchdog(mut self) -> WatchdogHandle {
        let mem_usage_bytes = Arc::new(AtomicUsize::new(0));
        let handle_mem_usage_bytes = mem_usage_bytes.clone();

        let join_handle = tokio::spawn(async move {
            mem_usage_bytes.store(0, Ordering::Relaxed);

            loop {
                select! {
                    _ = self.interval.tick() => {
                        let current_mem_usage_bytes = memory_stats::memory_stats()
                        .map(|s| s.physical_mem)
                        .unwrap_or(0);
                        mem_usage_bytes.store(current_mem_usage_bytes, Ordering::Relaxed);

                        if current_mem_usage_bytes > self.max_memory_usage_bytes {
                            std::thread::spawn(||{
                                // TODO: we should trigger manual flush and submission here
                                // wait 5 seconds to give metrics a chance to flush - then kill the process
                                std::thread::sleep(Duration::from_secs(5));
                                std::process::exit(1);
                            });
                            return
                        }

                    },
                    _ = self.shutdown_receiver.recv() => {
                        return
                    },
                }
            }
        });
        WatchdogHandle {
            handle: join_handle.map(Result::ok).boxed().shared(),
            mem_usage_bytes: handle_mem_usage_bytes,
        }
    }
}
