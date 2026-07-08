// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Forked-child setup; after fork the process is single-threaded, may inherit a corrupt heap, uses
//! only async-signal-safe syscalls, and every path ends in `exit_process`.

use core::ffi::c_char;
use core::ptr::null_mut;
use core::sync::atomic::Ordering;

use super::collect::{emit_report_to_fd, CrashEvent};
use super::sigaction::reset_signals_to_default;
use super::EXIT_CODE_FAILURE;
use crate::collector_signal_safe::{capabilities, config, state, sys};

#[derive(Clone, Copy)]
enum StdioFallback {
    CloseAll,
    LeaveOpen,
}

pub(super) fn receiver_child(read_fd: i32, write_fd: i32) -> ! {
    sys::close(write_fd);
    let read_fd = sanitize_forked_child(read_fd, StdioFallback::CloseAll);
    if read_fd < 0 {
        sys::exit_process(EXIT_CODE_FAILURE);
    }
    if read_fd != libc::STDIN_FILENO {
        let _ = sys::dup2(read_fd, libc::STDIN_FILENO);
        sys::close(read_fd);
    }
    if state::SETTINGS
        .close_fds_on_receiver
        .load(Ordering::Relaxed)
    {
        let _ = sys::close_range_from(libc::STDERR_FILENO + 1);
    }
    strip_loader_injection_env();

    let path = state::meta().process_path.as_slice();
    if path.is_empty() || path[path.len() - 1] != 0 {
        sys::exit_process(EXIT_CODE_FAILURE);
    }

    let argv = [path.as_ptr() as *const c_char, null_mut()];
    unsafe {
        libc::execv(path.as_ptr() as *const c_char, argv.as_ptr());
    }
    sys::exit_process(EXIT_CODE_FAILURE);
}

pub(super) fn collector_child(read_fd: i32, write_fd: i32, event: CrashEvent) -> ! {
    sys::close(read_fd);
    let write_fd = sanitize_forked_child(write_fd, StdioFallback::LeaveOpen);
    if write_fd < 0 {
        sys::exit_process(EXIT_CODE_FAILURE);
    }
    ignore_sigpipe();

    let _ = emit_report_to_fd(write_fd, event);
    sys::close(write_fd);
    sys::exit_process(0);
}

fn sanitize_forked_child(mut keep_fd: i32, fallback: StdioFallback) -> i32 {
    if (libc::STDIN_FILENO..=libc::STDERR_FILENO).contains(&keep_fd) {
        let relocated = sys::fcntl_dupfd(keep_fd, libc::STDERR_FILENO + 1);
        if relocated < 0 {
            return -1;
        }
        sys::close(keep_fd);
        keep_fd = relocated;
    }

    let _ = reset_signals_to_default(&config::CRASH_SIGNALS);
    disable_alt_stack();

    let devnull = if capabilities::has(capabilities::DEV_NULL) {
        sys::open_readwrite(c"/dev/null".as_ptr().cast())
    } else {
        -1
    };
    if devnull >= 0 {
        let _ = sys::dup2(devnull, libc::STDIN_FILENO);
        let _ = sys::dup2(devnull, libc::STDOUT_FILENO);
        let _ = sys::dup2(devnull, libc::STDERR_FILENO);
        if devnull > libc::STDERR_FILENO {
            sys::close(devnull);
        }
    } else if matches!(fallback, StdioFallback::CloseAll) {
        close_stdio();
    }
    keep_fd
}

fn close_stdio() {
    sys::close(libc::STDIN_FILENO);
    sys::close(libc::STDOUT_FILENO);
    sys::close(libc::STDERR_FILENO);
}

/// Unregister any alternate signal stack inherited by a forked child.
///
/// The child resets its crash handlers to `SIG_DFL`, so the alt stack is no longer needed. Dropping
/// it explicitly means that even if some inherited disposition were re-armed, the child can never
/// run a handler on a stack region whose contents we no longer maintain.
fn disable_alt_stack() {
    let stack = libc::stack_t {
        ss_sp: null_mut(),
        ss_flags: libc::SS_DISABLE,
        ss_size: 0,
    };
    let _ = unsafe { libc::sigaltstack(&stack, null_mut()) };
}

/// Ignore `SIGPIPE` in a collector child before it writes the report.
///
/// The child inherits the crashing process' `SIGPIPE` disposition, which is often `SIG_DFL`
/// (terminate). If the receiver closed the read end, we want the write to fail with `EPIPE` --
/// which [`FdSink`](crate::collector_signal_safe::sys::FdSink) already reports as an error --
/// rather than a `SIGPIPE` killing us in the middle of the report.
fn ignore_sigpipe() {
    let mut ign: libc::sigaction = unsafe { core::mem::zeroed() };
    ign.sa_sigaction = libc::SIG_IGN;
    unsafe {
        libc::sigemptyset(&mut ign.sa_mask);
        let _ = libc::sigaction(libc::SIGPIPE, &ign, null_mut());
    }
}

fn strip_loader_injection_env() {
    let env = sys::environ_ptr();
    if env.is_null() {
        return;
    }
    const PREFIXES: [&[u8]; 2] = [b"LD_PRELOAD=", b"LD_AUDIT="];
    unsafe {
        let mut src = env;
        let mut dst = env;
        while !(*src).is_null() {
            let entry = *src;
            let injected = PREFIXES.iter().any(|p| sys::cstr_has_prefix(entry, p));
            if !injected {
                *dst = entry;
                dst = dst.add(1);
            }
            src = src.add(1);
        }
        *dst = null_mut();
    }
}
