// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

use super::collector_manager::Collector;
use super::receiver_manager::Receiver;
use super::signal_handler_manager::chain_signal_handler;
use crate::crash_info::Metadata;
use crate::shared::configuration::CrashtrackerConfiguration;
use ddcommon::timeout::TimeoutManager;
use libc::{c_void, siginfo_t, ucontext_t};
use std::ptr;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64};

// Note that this file makes use the following async-signal safe functions in a signal handler.
// <https://man7.org/linux/man-pages/man7/signal-safety.7.html>
// - clock_gettime
// - close (although Rust may call `free` because we call the higher-level nix interface)
// - dup2
// - fork (on MacOS; Linux calls `fork()` directly as syscall)
// - kill
// - poll
// - raise
// - read
// - sigaction
// - write

// These represent data used by the crashtracker.
// Using mutexes inside a signal handler is not allowed, so use `AtomicPtr`
// instead to get atomicity.
// These should always be either: null_mut, or `Box::into_raw()`
// This means that we can always clean up the memory inside one of these using
// `Box::from_raw` to recreate the box, then dropping it.
static METADATA: AtomicPtr<(Metadata, String)> = AtomicPtr::new(ptr::null_mut());
static CONFIG: AtomicPtr<(CrashtrackerConfiguration, String)> = AtomicPtr::new(ptr::null_mut());

/// Updates the crashtracker metadata for this process
/// Metadata is stored in a global variable and sent to the crashtracking
/// receiver when a crash occurs.
///
/// PRECONDITIONS:
///     None
/// SAFETY:
///     Crash-tracking functions are not guaranteed to be reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function uses a swap on an atomic pointer.
pub fn update_metadata(metadata: Metadata) -> anyhow::Result<()> {
    let metadata_string = serde_json::to_string(&metadata)?;
    let box_ptr = Box::into_raw(Box::new((metadata, metadata_string)));
    let old = METADATA.swap(box_ptr, SeqCst);
    if !old.is_null() {
        // Safety: This can only come from a box above.
        unsafe {
            std::mem::drop(Box::from_raw(old));
        }
    }
    Ok(())
}

/// Updates the crashtracker config for this process
/// Config is stored in a global variable and sent to the crashtracking
/// receiver when a crash occurs.
///
/// PRECONDITIONS:
///     None
/// SAFETY:
///     Crash-tracking functions are not guaranteed to be reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function uses a swap on an atomic pointer.
pub fn update_config(config: CrashtrackerConfiguration) -> anyhow::Result<()> {
    let config_string = serde_json::to_string(&config)?;
    let box_ptr = Box::into_raw(Box::new((config, config_string)));
    let old = CONFIG.swap(box_ptr, SeqCst);
    if !old.is_null() {
        // Safety: This can only come from a box above.
        unsafe {
            std::mem::drop(Box::from_raw(old));
        }
    }
    Ok(())
}

pub(crate) extern "C" fn handle_posix_sigaction(
    signum: i32,
    sig_info: *mut siginfo_t,
    ucontext: *mut c_void,
) {
    // Handle the signal.  Note this has a guard to ensure that we only generate
    // one crash report per process.
    let _ = handle_posix_signal_impl(sig_info, ucontext as *mut ucontext_t);
    // SAFETY: No preconditions.
    unsafe { chain_signal_handler(signum, sig_info, ucontext) };
}

static ENABLED: AtomicBool = AtomicBool::new(true);

/// Disables the crashtracker.
/// Note that this does not restore the old signal handlers, but rather turns crash-tracking into a
/// no-op, and then chains the old handlers.  This means that handlers registered after the
/// crashtracker will continue to work as expected.
///
/// # Preconditions
///   None
/// # Safety
///   None
/// # Atomicity
///   This function is atomic and idempotent.  Calling it multiple times is allowed.
pub fn disable() {
    ENABLED.store(false, SeqCst);
}

/// Enables the crashtracker, if had been previously disabled.
/// If crashtracking has not been initialized, this function will have no effect.
///
/// # Preconditions
///   None
/// # Safety
///   None
/// # Atomicity
///   This function is atomic and idempotent.  Calling it multiple times is allowed.
pub fn enable() {
    ENABLED.store(true, SeqCst);
}

fn handle_posix_signal_impl(
    sig_info: *const siginfo_t,
    ucontext: *const ucontext_t,
) -> anyhow::Result<()> {
    if !ENABLED.load(SeqCst) {
        return Ok(());
    }

    // If this code hits a stack overflow, then it will result in a segfault.  That situation is
    // protected by the one-time guard.

    // One-time guard to guarantee at most one crash per process
    static NUM_TIMES_CALLED: AtomicU64 = AtomicU64::new(0);
    if NUM_TIMES_CALLED.fetch_add(1, SeqCst) > 0 {
        // In the case where some lower-level signal handler recovered the error
        // we don't want to spam the system with calls.  Make this one shot.
        return Ok(());
    }

    // Leak config and metadata to avoid calling `drop` during a crash
    // Note that these operations also replace the global states.  When the one-time guard is
    // passed, all global configuration and metadata becomes invalid.
    let config_ptr = CONFIG.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(!config_ptr.is_null(), "No crashtracking config");
    let (config, config_str) = unsafe { &*config_ptr };

    let metadata_ptr = METADATA.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(!metadata_ptr.is_null(), "No crashtracking metadata");
    let (_metadata, metadata_string) = unsafe { &*metadata_ptr };

    let timeout_manager = TimeoutManager::new(config.timeout());

    // Optionally, create the receiver.  This all hinges on whether or not the configuration has a
    // non-null unix domain socket specified.  If it doesn't, then we need to check the receiver
    // configuration.  If it does, then we just connect to the socket.
    let unix_socket_path = config.unix_socket_path().as_deref().unwrap_or_default();

    let receiver = if unix_socket_path.is_empty() {
        Receiver::spawn_from_stored_config()?
    } else {
        Receiver::from_socket(unix_socket_path)?
    };

    let collector = Collector::spawn(
        &receiver,
        config,
        config_str,
        metadata_string,
        sig_info,
        ucontext,
    )
    .map_err(anyhow::Error::new)?;

    // We're done. Wrap up our interaction with the receiver.
    collector.finish(&timeout_manager);
    receiver.finish(&timeout_manager);

    Ok(())
}
