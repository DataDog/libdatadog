// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

use super::collector_manager::Collector;
use super::receiver_manager::Receiver;
use super::signal_handler_manager::chain_signal_handler;
use crate::crash_info::Metadata;
use crate::shared::configuration::CrashtrackerConfiguration;
use libc::{c_void, siginfo_t, ucontext_t};
use libdd_common::timeout::TimeoutManager;
use std::panic;
use std::panic::PanicHookInfo;
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
static PANIC_MESSAGE: AtomicPtr<String> = AtomicPtr::new(ptr::null_mut());

type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync>;
static PREVIOUS_PANIC_HOOK: AtomicPtr<PanicHook> = AtomicPtr::new(ptr::null_mut());

#[derive(Debug, thiserror::Error)]
pub enum CrashHandlerError {
    #[error("No crashtracking config available")]
    NoConfig,
    #[error("No crashtracking metadata available")]
    NoMetadata,
    #[error("Failed to spawn receiver: {0}")]
    ReceiverSpawnError(#[from] super::receiver_manager::ReceiverError),
    #[error("Failed to spawn collector: {0}")]
    CollectorSpawnError(#[from] super::collector_manager::CollectorSpawnError),
}

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

/// Register the panic hook.
///
/// This function is used to register the panic hook and store the previous hook.
/// PRECONDITIONS:
///     None
/// SAFETY:
///     Crash-tracking functions are not guaranteed to be reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function uses a swap on an atomic pointer.
pub fn register_panic_hook() -> anyhow::Result<()> {
    // register only once, if it is already registered, do nothing
    if !PREVIOUS_PANIC_HOOK.load(SeqCst).is_null() {
        return Ok(());
    }

    let old_hook = panic::take_hook();
    let old_hook_ptr = Box::into_raw(Box::new(old_hook));
    PREVIOUS_PANIC_HOOK.swap(old_hook_ptr, SeqCst);
    panic::set_hook(Box::new(|panic_info| {
        if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            let message_ptr = PANIC_MESSAGE.swap(Box::into_raw(Box::new(s.to_string())), SeqCst);
            // message_ptr should be null, but just in case.
            if !message_ptr.is_null() {
                unsafe {
                    std::mem::drop(Box::from_raw(message_ptr));
                }
            }
        }
        call_previous_panic_hook(panic_info);
    }));
    Ok(())
}

/// Call the previous panic hook.
///
/// This function is used to call the previous panic hook.
/// PRECONDITIONS:
///     None
/// SAFETY:
///     Crash-tracking functions are not guaranteed to be reentrant.
///     No other crash-handler functions should be called concurrently.
fn call_previous_panic_hook(panic_info: &PanicHookInfo<'_>) {
    let old_hook_ptr = PREVIOUS_PANIC_HOOK.load(SeqCst);
    if !old_hook_ptr.is_null() {
        // Safety: This pointer can only come from Box::into_raw above in register_panic_hook.
        // We borrow it here without taking ownership so it remains valid for future calls.
        unsafe {
            let old_hook = &*old_hook_ptr;
            old_hook(panic_info);
        }
    }
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
) -> Result<(), CrashHandlerError> {
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
    if config_ptr.is_null() {
        return Err(CrashHandlerError::NoConfig);
    }
    let (config, config_str) = unsafe { &*config_ptr };

    let metadata_ptr = METADATA.swap(ptr::null_mut(), SeqCst);
    if metadata_ptr.is_null() {
        return Err(CrashHandlerError::NoMetadata);
    }
    let (_metadata, metadata_string) = unsafe { &*metadata_ptr };

    // Get the panic message pointer but don't dereference or deallocate in signal handler.
    // The collector child process will handle converting this to a String after forking.
    // Leak of the message pointer is ok here.
    let message_ptr = PANIC_MESSAGE.swap(ptr::null_mut(), SeqCst);

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
        message_ptr,
        sig_info,
        ucontext,
    )?;

    // We're done. Wrap up our interaction with the receiver.
    collector.finish(&timeout_manager);
    receiver.finish(&timeout_manager);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_panic_hook() {
        assert!(PREVIOUS_PANIC_HOOK.load(SeqCst).is_null());

        let result = register_panic_hook();
        assert!(result.is_ok());

        assert!(!PREVIOUS_PANIC_HOOK.load(SeqCst).is_null());
    }

    #[test]
    fn test_panic_message_storage_and_retrieval() {
        // Test that panic messages can be stored and retrieved via atomic pointer
        let test_message = "test panic message".to_string();
        let message_ptr = Box::into_raw(Box::new(test_message.clone()));

        // Store the message
        let old_ptr = PANIC_MESSAGE.swap(message_ptr, SeqCst);
        assert!(old_ptr.is_null()); // Should be null initially

        // Retrieve and verify
        let retrieved_ptr = PANIC_MESSAGE.swap(ptr::null_mut(), SeqCst);
        assert!(!retrieved_ptr.is_null());

        unsafe {
            let retrieved_message = *Box::from_raw(retrieved_ptr);
            assert_eq!(retrieved_message, test_message);
        }
    }

    #[test]
    fn test_panic_message_null_handling() {
        // Test that null message pointers are handled correctly
        PANIC_MESSAGE.store(ptr::null_mut(), SeqCst);

        let message_ptr = PANIC_MESSAGE.load(SeqCst);
        assert!(message_ptr.is_null());

        // Swapping null with null should be safe
        let old_ptr = PANIC_MESSAGE.swap(ptr::null_mut(), SeqCst);
        assert!(old_ptr.is_null());
    }

    #[test]
    fn test_panic_message_replacement() {
        // Test that replacing an existing message cleans up the old one
        let message1 = "first message".to_string();
        let message2 = "second message".to_string();

        let ptr1 = Box::into_raw(Box::new(message1));
        let ptr2 = Box::into_raw(Box::new(message2.clone()));

        PANIC_MESSAGE.store(ptr1, SeqCst);
        let old_ptr = PANIC_MESSAGE.swap(ptr2, SeqCst);

        // Old pointer should be the first one
        assert_eq!(old_ptr, ptr1);

        // Clean up both
        unsafe {
            drop(Box::from_raw(old_ptr));
            let final_ptr = PANIC_MESSAGE.swap(ptr::null_mut(), SeqCst);
            let final_message = *Box::from_raw(final_ptr);
            assert_eq!(final_message, message2);
        }
    }

    #[test]
    fn test_metadata_update_atomic() {
        // Test that metadata updates are atomic
        let metadata = Metadata {
            library_name: "test".to_string(),
            library_version: "1.0.0".to_string(),
            family: "test_family".to_string(),
            tags: vec![],
        };

        let result = update_metadata(metadata.clone());
        assert!(result.is_ok());

        // Verify metadata was stored
        let metadata_ptr = METADATA.load(SeqCst);
        assert!(!metadata_ptr.is_null());

        unsafe {
            let (stored_metadata, _) = &*metadata_ptr;
            assert_eq!(stored_metadata.library_name, "test");
        }
    }
}
