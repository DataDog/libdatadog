// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Signal handler lifecycle and crash-path orchestration.
//!
//! The installed handler records the signal event, starts a receiver child connected by a pipe,
//! starts a collector child to stack-walk and write the signal-safe wire report, then reaps both
//! children before chaining or refaulting. Any missing capability or crash-path failure falls back
//! to `report_fd` when the caller supplied a valid descriptor.

mod child;
mod collect;
mod crash;
mod lifecycle;
mod sigaction;

use core::sync::atomic::Ordering;

use super::fmt::{write_i32, I32_BUF_CAPACITY};
use super::state;
use super::sys::FdSink;

pub use lifecycle::{bootstrap_complete, init, shutdown, InitResult};

// Used only by forked children; 125 matches the existing shell-like "cannot exec" convention.
pub(super) const EXIT_CODE_FAILURE: i32 = 125;

fn crash_debug(msg: &[u8], sig: i32) {
    if !state::SETTINGS.debug_log.load(Ordering::Relaxed) {
        return;
    }
    let mut sink = FdSink::new(libc::STDERR_FILENO);
    let _ = super::Sink::put(&mut sink, b"dd-crashtracker[signal-safe]: ");
    let _ = super::Sink::put(&mut sink, msg);
    if sig >= 0 {
        let _ = super::Sink::put(&mut sink, b" ");
        let mut buf = [0u8; I32_BUF_CAPACITY];
        let written = write_i32(sig, &mut buf);
        let _ = super::Sink::put(&mut sink, &buf[..written]);
    }
    let _ = super::Sink::put(&mut sink, b"\n");
}
