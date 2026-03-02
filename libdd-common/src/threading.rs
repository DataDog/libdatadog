// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// Returns a numeric identifier for the current OS thread.
#[cfg(target_os = "linux")]
pub fn get_current_thread_id() -> i64 {
    // SAFETY: syscall(SYS_gettid) has no preconditions for current thread.
    unsafe { libc::syscall(libc::SYS_gettid) as i64 }
}

/// Returns a numeric identifier for the current OS thread.
#[cfg(target_os = "macos")]
pub fn get_current_thread_id() -> i64 {
    let mut tid: u64 = 0;
    // SAFETY: `pthread_threadid_np` has no preconditions for current thread
    // when pthread_t is 0 and output pointer is valid.
    let rc = unsafe { libc::pthread_threadid_np(0, &mut tid) };
    debug_assert_eq!(
        rc,
        0,
        "pthread_threadid_np failed: {rc} ({})",
        std::io::Error::from_raw_os_error(rc)
    );
    tid as i64
}

/// Returns a numeric identifier for the current OS thread.
#[cfg(target_os = "windows")]
pub fn get_current_thread_id() -> i64 {
    // SAFETY: GetCurrentThreadId has no preconditions.
    unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() as i64 }
}

/// Returns a numeric identifier for the current OS thread.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!("libdd_common::threading::get_current_thread_id is unsupported on this platform");
