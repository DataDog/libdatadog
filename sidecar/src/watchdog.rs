// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use futures::{
    future::{BoxFuture, Shared},
    FutureExt,
};
use std::{
    sync::{
        atomic::{AtomicU32, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use crate::service::SidecarServer;
use tokio::{select, sync::mpsc::Receiver};
use tracing::error;

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
            interval: tokio::time::interval(Duration::from_secs(10)),
            max_memory_usage_bytes: 1024 * 1024 * 1024, // 1 GB
            shutdown_receiver,
        }
    }

    pub fn spawn_watchdog(mut self, server: SidecarServer) -> WatchdogHandle {
        let mem_usage_bytes = Arc::new(AtomicUsize::new(0));
        let handle_mem_usage_bytes = mem_usage_bytes.clone();

        let still_alive = Arc::new(AtomicU32::new(0));
        let still_alive_thread = still_alive.clone();

        const SHUTDOWN: u32 = u32::MAX;

        let interval = self.interval.period();
        std::thread::spawn(move || {
            let mut maybe_stuck = false;
            let mut last = 0;
            loop {
                std::thread::sleep(interval);
                let current = still_alive_thread.load(Ordering::Relaxed);
                if last != current {
                    if current == SHUTDOWN {
                        return;
                    }
                    last = current;
                    maybe_stuck = false;
                } else {
                    if maybe_stuck {
                        std::thread::spawn(move || {
                            error!("Watchdog timeout: Sidecar stuck for at least {} seconds. Sending SIGABRT, possibly dumping core.", interval.as_secs());
                        });
                        // wait 1 seconds to give log a chance to flush - then kill the process
                        std::thread::sleep(Duration::from_secs(1));
                        unsafe { libc::abort() };
                    }
                    maybe_stuck = true;
                }
            }
        });

        let join_handle = tokio::spawn(async move {
            mem_usage_bytes.store(0, Ordering::Relaxed);

            loop {
                select! {
                    _ = self.interval.tick() => {
                        still_alive.fetch_add(1, Ordering::Relaxed);

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

                            error!("Watchdog memory exceeded: Sidecar using more than {} bytes. Exiting.", self.max_memory_usage_bytes);
                            error!("Memory statistics: {:?}", server.compute_stats().await);
                            return
                        }

                    },
                    _ = self.shutdown_receiver.recv() => {
                        still_alive.store(SHUTDOWN, Ordering::Relaxed);
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
