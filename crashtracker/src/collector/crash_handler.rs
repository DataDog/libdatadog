// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

use super::emitters::emit_crashreport;
use super::receiver_manager::{
    has_receiver_config, make_receiver, receiver_finish, receiver_from_socket,
};
use super::saguard::SaGuard;
use super::signal_handler_manager::chain_signal_handler;
use crate::crash_info::Metadata;
use crate::shared::configuration::CrashtrackerConfiguration;
use anyhow::Context;
use libc::{c_void, siginfo_t, ucontext_t};
use nix::sys::signal;
use std::io::Write;
use std::ptr;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicPtr, AtomicU64};
use std::time::Instant;

// Note that this file makes use the following async-signal safe functions in a signal handler.
// <https://man7.org/linux/man-pages/man7/signal-safety.7.html>
// - clock_gettime
// - close (although Rust may call `free` because we call the higher-level nix interface)
// - dup2
// - fork (but specifically only because it does so without calling atfork handlers)
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

fn handle_posix_signal_impl(
    sig_info: *const siginfo_t,
    ucontext: *const ucontext_t,
) -> anyhow::Result<()> {
    // If this is a SIGSEGV signal, it could be called due to a stack overflow. In that case, since
    // this signal allocates to the stack and cannot guarantee it is running without SA_NODEFER, it
    // is possible that we will re-emit the signal. Contemporary unices handle this just fine (no
    // deadlock), but it does mean we will fail.  Currently this situation is not detected.
    // In general, handlers do not know their own stack usage requirements in advance and are
    // incapable of guaranteeing that they will not overflow the stack.

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
    // In a perfet world, we'd also grab the receiver config in this section, but since the
    // execution forks based on whether or not the receiver is configured, we check that later.
    let config = CONFIG.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(!config.is_null(), "No crashtracking config");
    let (config, config_str) = unsafe { config.as_ref().context("No crashtracking receiver")? };

    let metadata_ptr = METADATA.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(!metadata_ptr.is_null(), "No crashtracking metadata");
    let (_metadata, metadata_string) = unsafe { metadata_ptr.as_ref().context("metadata ptr")? };
    anyhow::ensure!(has_receiver_config(), "No receiver config");

    // Since we've gotten this far, we're going to start working on the crash report. This
    // operation needs to be mindful of the total walltime elapsed during handling. This isn't only
    // to prevent hanging, but also because services capable of restarting after a crash experience
    // crashes as probabalistic queue-holding events, and so crash handling represents dead time
    // which makes the overall service increasingly incompetent at handling load.
    let timeout_ms = config.timeout_ms();
    let start_time = Instant::now(); // This is the time at which the signal was received

    // During the execution of this signal handler, block ALL other signals, especially because we
    // cannot control whether or not we run with SA_NODEFER (crashtracker might have been chained).
    // The especially problematic signals are SIGCHLD and SIGPIPE, which are possibly delivered due
    // to the execution of this handler.
    // SaGuard ensures that signals are restored to their original state even if control flow is
    // disrupted.
    let _guard = SaGuard::<2>::new(&[signal::SIGCHLD, signal::SIGPIPE])?;

    // Optionally, create the receiver.  This all hinges on whether or not the configuration has a
    // non-null unix domain socket specified.  If it doesn't, then we need to check the receiver
    // configuration.  If it does, then we just connect to the socket.
    let unix_socket_path = config.unix_socket_path().clone().unwrap_or_default();

    let mut receiver = if !unix_socket_path.is_empty() {
        receiver_from_socket(&unix_socket_path)?
    } else {
        make_receiver()?
    };

    // No matter how the receiver was created, attach to its stream
    // Safety: the receiver was just created, and we haven't closed its FD.
    let mut unix_stream = unsafe { receiver.receiver_unix_stream() };

    // Currently the emission of the crash report doesn't have a firm time guarantee
    // In a future patch, the timeout parameter should be passed into the IPC loop here and
    // checked periodically.
    let res = emit_crashreport(
        &mut unix_stream,
        config,
        config_str,
        metadata_string,
        sig_info,
        ucontext,
    );

    let _ = unix_stream.flush();
    unix_stream
        .shutdown(std::net::Shutdown::Write)
        .context("Could not shutdown writing on the stream")?;

    // We're done. Wrap up our interaction with the receiver.
    receiver_finish(receiver, start_time, timeout_ms);

    res
}
