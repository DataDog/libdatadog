// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Attaches OTel thread contexts on three threads before crashing so the
//! receiver can read them via ptrace + `/proc/<pid>/mem`.

use crate::modes::behavior::Behavior;
use libdd_crashtracker::CrashtrackerConfiguration;
use std::path::Path;

pub const MAIN_TRACE_ID: &str = "0123456789abcdeffedcba9876543210";
pub const MAIN_SPAN_ID: &str = "aabbccdd11223344";
pub const WORKER_HTTP_TRACE_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const WORKER_DB_TRACE_ID: &str = "cccccccccccccccccccccccccccccccccc";

pub struct Test;

impl Behavior for Test {
    fn setup(
        &self,
        _output_dir: &Path,
        config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        config.set_collect_all_threads(true);
        config.set_collect_otel_thread_contexts(true);
        Ok(())
    }

    fn pre(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn post(&self, _output_dir: &Path) -> anyhow::Result<()> {
        #[cfg(target_os = "linux")]
        {
            use libdd_otel_thread_ctx::linux::OwnedThreadContext;

            // Spawn two worker threads with their own contexts, blocked on a barrier.
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));

            let b = barrier.clone();
            std::thread::Builder::new()
                .name("worker-http".into())
                .spawn(move || {
                    let ctx = OwnedThreadContext::new(
                        [0xAA; 16],
                        [0xBB; 8],
                        0x01,
                        [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88],
                        &[],
                    );
                    let _prev = ctx.attach();
                    b.wait();
                })?;

            let b = barrier.clone();
            std::thread::Builder::new()
                .name("worker-db".into())
                .spawn(move || {
                    let ctx = OwnedThreadContext::new(
                        [0xCC; 16],
                        [0xDD; 8],
                        0x01,
                        [0xEE, 0xFF, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55],
                        &[],
                    );
                    let _prev = ctx.attach();
                    b.wait();
                })?;

            // Attach context on the main (crashing) thread.
            let ctx = OwnedThreadContext::new(
                [
                    0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xdc, 0xba, 0x98, 0x76,
                    0x54, 0x32, 0x10,
                ],
                [0xaa, 0xbb, 0xcc, 0xdd, 0x11, 0x22, 0x33, 0x44],
                0x01,
                [0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe, 0xba, 0xbe],
                &[],
            );
            let _prev = ctx.attach();
            // Leak: contexts must stay alive until the crash.
        }
        Ok(())
    }
}
