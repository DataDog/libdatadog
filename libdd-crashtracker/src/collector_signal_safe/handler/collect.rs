// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Crash-event capture, report emission, and child reaping.

use core::ffi::{c_int, c_void};
use core::sync::atomic::Ordering;

use super::child::{collector_child, receiver_child};
use super::crash_debug;
use super::sigaction::{siginfo_addr, siginfo_pid};
use super::EXIT_CODE_FAILURE;
use crate::collector_signal_safe::sys::FdSink;
use crate::collector_signal_safe::{
    backtrace, capabilities, config, state, sys, CrashContext, Report, SignalInfo,
};

const REAP_KILL_TIMEOUT_MS: i64 = 500;
const REAP_WAIT_INTERVAL_MS: i32 = 100;

#[derive(Clone, Copy)]
pub(super) struct CrashEvent {
    pub(super) sig: i32,
    pub(super) si_code: i32,
    pub(super) has_info: bool,
    pub(super) si_addr: usize,
    pub(super) si_pid: i32,
    pub(super) pid: i32,
    pub(super) tid: i32,
    ucontext: *mut c_void,
}

impl CrashEvent {
    pub(super) fn from_signal(
        sig: c_int,
        info: *mut libc::siginfo_t,
        ucontext: *mut c_void,
    ) -> Self {
        let has_info = !info.is_null();
        let si_code = if has_info {
            unsafe { (*info).si_code }
        } else {
            0
        };
        let si_addr = if has_info {
            unsafe { siginfo_addr(info) }
        } else {
            0
        };
        let si_pid = if has_info {
            unsafe { siginfo_pid(info) }
        } else {
            0
        };

        Self {
            sig,
            si_code,
            has_info,
            si_addr,
            si_pid,
            pid: sys::getpid(),
            tid: sys::gettid(),
            ucontext,
        }
    }

    fn context<'a>(self, frames: &'a [usize]) -> CrashContext<'a> {
        CrashContext {
            signal: SignalInfo::new(self.sig, self.si_code, self.si_addr, self.has_info),
            pid: self.pid,
            tid: self.tid,
            frames,
        }
    }

    pub(super) fn instruction_pointer(self) -> usize {
        backtrace::instruction_pointer(self.ucontext)
    }
}

pub(super) fn collect_crash(event: CrashEvent) {
    // The receiver is forked first and execs the configured receiver binary with the read side of
    // the pipe on stdin. Report generation runs in a second forked child so stack walking and
    // formatting happen outside the crashing process' possibly-corrupt heap. The original process
    // only forks, closes fds, and reaps. A receiver exit status of `EXIT_CODE_FAILURE` means exec
    // failed, so the handler can still fall back to `report_fd`.
    let report_fd = state::SETTINGS.report_fd.load(Ordering::Relaxed);
    let caps = capabilities::get();

    if !caps.contains(capabilities::FORK_OK) {
        crash_debug(b"fork unavailable", event.sig);
        fallback_to_report_fd(event, caps, report_fd, capabilities::DEGRADED_NO_FORK);
        return;
    }

    if !caps.contains(capabilities::RECEIVER_OK) {
        crash_debug(b"receiver unavailable", event.sig);
        fallback_to_report_fd(
            event,
            caps,
            report_fd,
            capabilities::DEGRADED_RECEIVER_UNAVAILABLE,
        );
        return;
    }

    if !caps.contains(capabilities::PIPE_OK) {
        crash_debug(b"pipe unavailable", event.sig);
        fallback_to_report_fd(event, caps, report_fd, capabilities::DEGRADED_NO_PIPE);
        return;
    }

    let Some(pipe) = sys::pipe() else {
        crash_debug(b"pipe failed", event.sig);
        fallback_to_report_fd(event, caps, report_fd, capabilities::DEGRADED_PIPE_FAILED);
        return;
    };

    let receiver = unsafe { sys::fork_raw() };
    if receiver == 0 {
        receiver_child(pipe.read, pipe.write);
    }
    if receiver < 0 {
        crash_debug(b"receiver fork failed", event.sig);
        sys::close(pipe.read);
        sys::close(pipe.write);
        fallback_to_report_fd(event, caps, report_fd, capabilities::DEGRADED_FORK_FAILED);
        return;
    }

    let collector = unsafe { sys::fork_raw() };
    if collector == 0 {
        collector_child(pipe.read, pipe.write, event);
    }

    sys::close(pipe.read);
    sys::close(pipe.write);

    if collector > 0 {
        let _ = reap_or_kill(
            collector as i32,
            state::SETTINGS.collector_reap_ms.load(Ordering::Relaxed) as i64,
        );
    } else {
        crash_debug(b"collector fork failed", event.sig);
        fallback_to_report_fd(event, caps, report_fd, capabilities::DEGRADED_FORK_FAILED);
    }

    let receiver_status = reap_or_kill(
        receiver as i32,
        state::SETTINGS.receiver_reap_ms.load(Ordering::Relaxed) as i64,
    );
    if receiver_status.is_some_and(|status| exited_with(status, EXIT_CODE_FAILURE)) {
        crash_debug(b"receiver exec failed", event.sig);
        fallback_to_report_fd(
            event,
            caps,
            report_fd,
            capabilities::DEGRADED_RECEIVER_UNAVAILABLE,
        );
    }
}

fn fallback_to_report_fd(
    event: CrashEvent,
    caps: capabilities::Capabilities,
    report_fd: i32,
    reason: capabilities::Degradations,
) {
    capabilities::note_degraded(reason);
    if caps.contains(capabilities::REPORT_FD_OK) {
        capabilities::note_degraded(capabilities::DEGRADED_REPORT_TO_FD);
        // The fallback path does not own `report_fd`; the caller keeps it open.
        let _ = emit_report_to_fd(report_fd, event);
    }
}

pub(super) fn emit_report_to_fd(write_fd: i32, event: CrashEvent) -> bool {
    let mut frames = [0usize; config::BACKTRACE_LEVELS_MAX];
    let max_frames = state::SETTINGS
        .max_frames
        .load(Ordering::Relaxed)
        .min(config::BACKTRACE_LEVELS_MAX);
    let caps = capabilities::get();
    let can_walk = caps.contains(capabilities::PROC_VM_READV);
    let n = backtrace::backtrace_from_ucontext(
        &mut frames[..max_frames],
        event.ucontext,
        event.pid,
        can_walk,
    );
    let stackwalk_method = if n == 0 {
        "none"
    } else if can_walk {
        "fp_pvr"
    } else {
        "seed_only"
    };

    let meta = state::meta();
    let report = Report {
        config_json: meta.config_json.as_str(),
        library_name: meta.library_name.as_str(),
        library_version: meta.library_version.as_str(),
        family: meta.family.as_str(),
        default_service: meta.default_service.as_str(),
        service: meta.service.as_str(),
        env: meta.env.as_str(),
        app_version: meta.app_version.as_str(),
        runtime_id: meta.runtime_id.as_str(),
        platform: meta.platform.as_str(),
        stackwalk_method,
        capabilities: caps,
        degradations: capabilities::degradations(),
    };
    let context = event.context(&frames[..n]);

    let mut sink = FdSink::new(write_fd);
    let emitted = crate::collector_signal_safe::emit_report(&mut sink, &report, &context);
    // Flush the staged bytes before the fd is closed, otherwise the report is lost on close.
    let flushed = sink.flush();
    emitted && flushed
}

fn reap_or_kill(pid: i32, timeout_ms: i64) -> Option<i32> {
    match sys::reap_child(pid, timeout_ms, REAP_WAIT_INTERVAL_MS, REAP_KILL_TIMEOUT_MS) {
        sys::ChildReap::Reaped(status) => Some(status),
        sys::ChildReap::WaitFailed(_) => {
            crash_debug(b"waitpid failed", -1);
            None
        }
        sys::ChildReap::NoChild | sys::ChildReap::TimedOut => None,
    }
}

fn exited_with(status: i32, code: i32) -> bool {
    libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == code
}
