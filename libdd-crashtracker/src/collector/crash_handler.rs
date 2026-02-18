// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

use super::collector_manager::Collector;
use super::receiver_manager::Receiver;
use super::signal_handler_manager::chain_signal_handler;
use crate::crash_info::Metadata;
use crate::shared::configuration::CrashtrackerConfiguration;
use crate::StackTrace;
use libc::{c_void, siginfo_t, ucontext_t};
use libdd_common::timeout::TimeoutManager;
use std::os::fd::OwnedFd;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixStream;
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

/// Format a panic message with optional location information.
fn format_message(
    category: &str,
    panic_message: &str,
    location: Option<&panic::Location>,
) -> String {
    let base = if panic_message.is_empty() {
        format!("Process panicked with {}", category)
    } else {
        format!("Process panicked with {} \"{}\"", category, panic_message)
    };

    match location {
        Some(loc) => format!("{} ({}:{}:{})", base, loc.file(), loc.line(), loc.column()),
        None => base,
    }
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
        // Extract panic message from payload (supports &str and String)
        let message = if let Some(&s) = panic_info.payload().downcast_ref::<&str>() {
            format_message("message", s, panic_info.location())
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            format_message("message", s.as_str(), panic_info.location())
        } else {
            // For non-string types, use a generic message
            format_message("unknown type", "", panic_info.location())
        };

        // Store the message, cleaning up any old message
        let message_ptr = PANIC_MESSAGE.swap(Box::into_raw(Box::new(message)), SeqCst);
        // message_ptr should be null, but just in case.
        if !message_ptr.is_null() {
            unsafe {
                std::mem::drop(Box::from_raw(message_ptr));
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

    // Mark this process as a collector for the preload logger
    #[cfg(target_os = "linux")]
    {
        super::api::mark_preload_logger_collector();
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

    // Take config and metadata out of global storage.
    // We borrow via raw pointer and intentionally leak (do not reconstruct the Box) to avoid
    // calling `drop`, and therefore `free`, inside a signal handler, which is not
    // async-signal-safe.  Once the one-time guard is passed, this storage is never updated again.
    let config_ptr = take_config_ptr();
    if config_ptr.is_null() {
        return Err(CrashHandlerError::NoConfig);
    }
    let (config, config_str) = unsafe { &*config_ptr };

    let metadata_ptr = take_metadata_ptr();
    if metadata_ptr.is_null() {
        return Err(CrashHandlerError::NoMetadata);
    }
    let (_metadata, metadata_string) = unsafe { &*metadata_ptr };

    // Get the panic message pointer but don't dereference or deallocate in signal handler.
    // The collector child process will handle converting this to a String after forking.
    // Leak of the message pointer is ok here.
    let message_ptr = PANIC_MESSAGE.swap(ptr::null_mut(), SeqCst);

    let timeout_manager = TimeoutManager::new(config.timeout());

    let receiver = Receiver::from_crashtracker_config(config)?;

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

/// Atomically swaps the metadata pointer to null and returns the old raw pointer.
/// Async-signal-safe (only performs an atomic swap).
///
/// Callers are responsible for the returned memory:
/// - Signal handlers: borrow via `&*ptr` and intentionally leak (avoids signal-unsafe `free`).
fn take_metadata_ptr() -> *mut (crate::crash_info::Metadata, String) {
    METADATA.swap(ptr::null_mut(), SeqCst)
}

/// Atomically swaps the config pointer to null and returns the old raw pointer.
/// Async-signal-safe (only performs an atomic swap).
///
/// Callers are responsible for the returned memory:
/// - Signal handlers: borrow via `&*ptr` and intentionally leak (avoids signal-unsafe `free`).
fn take_config_ptr() -> *mut (
    crate::shared::configuration::CrashtrackerConfiguration,
    String,
) {
    CONFIG.swap(ptr::null_mut(), SeqCst)
}

/// Takes the current metadata out of global storage, leaving it unset.
/// The returned value is properly owned and will be dropped by the caller.
/// Do NOT call from a signal handler; use `take_metadata_ptr` instead.
fn take_metadata() -> Option<(crate::crash_info::Metadata, String)> {
    let ptr = take_metadata_ptr();
    if ptr.is_null() {
        None
    } else {
        // Safety: ptr was created by Box::into_raw in update_metadata
        Some(*unsafe { Box::from_raw(ptr) })
    }
}

/// Takes the current config out of global storage, leaving it unset.
/// The returned value is properly owned and will be dropped by the caller.
/// Do NOT call from a signal handler; use `take_config_ptr` instead.
fn take_config() -> Option<(
    crate::shared::configuration::CrashtrackerConfiguration,
    String,
)> {
    let ptr = take_config_ptr();
    if ptr.is_null() {
        None
    } else {
        // Safety: ptr was created by Box::into_raw in update_config
        Some(*unsafe { Box::from_raw(ptr) })
    }
}

/// This function is designed to be when a program is at a terminal state
/// and the application wants to report an unhandled exception to the crashtracker
///
/// Preconditions:
/// - The crashtracker must be started
/// - The stacktrace must be valid
///
/// This function will spawn the receiver process and call an emit function to pipe over
/// the crash data. We don't use the collector process because we are not in a signal handler
/// Rather, we call emit_crashreport directly and pipe over data to the receiver
pub fn report_unhandled_exception(
    exception_type: Option<&str>,
    exception_message: Option<&str>,
    stacktrace: StackTrace,
) -> Result<(), CrashHandlerError> {
    // Turn crashtracker off to prevent a recursive crash report emission
    // We do not turn it back on because this function is not intended to be used as
    // a recurring mechanism to report exceptions. We expect the application to exit
    // after
    disable();

    let (config, config_str) = take_config().ok_or(CrashHandlerError::NoConfig)?;
    let (_metadata, metadata_str) = take_metadata().ok_or(CrashHandlerError::NoMetadata)?;

    let receiver = Receiver::from_crashtracker_config(&config)?;

    let timeout_manager = TimeoutManager::new(config.timeout());

    let pid = unsafe { libc::getpid() };
    let tid = libdd_common::threading::get_current_thread_id() as libc::pid_t;

    let error_type_str = exception_type.unwrap_or("<unknown>");
    let error_message_str = exception_message.unwrap_or("<no message>");
    let message = format!(
        "Process was terminated due to an unhandled exception of type '{error_type_str}'. \
         Message: \"{error_message_str}\""
    );

    let message_ptr = Box::into_raw(Box::new(message));

    // Duplicate the socket fd before handing it to UnixStream so we retain an fd to poll on after
    // the write end is closed.  OwnedFd is the scope guard: it closes poll_fd on any exit path.
    //
    // SAFETY: dup() returns a fresh fd; we are its sole owner.  ProcessHandle only polls it
    // (wait_for_pollhup) and has no Drop impl, so it never closes the fd. Closing it here
    // after finish() returns is the first and only close
    let poll_fd = unsafe { OwnedFd::from_raw_fd(libc::dup(receiver.handle.uds_fd)) };
    let receiver_pid = receiver.handle.pid;

    {
        let mut unix_stream = unsafe { UnixStream::from_raw_fd(receiver.handle.uds_fd) };
        let _ = super::emitters::emit_crashreport(
            &mut unix_stream,
            &config,
            &config_str,
            &metadata_str,
            message_ptr,
            super::emitters::CrashKindData::UnhandledException { stacktrace },
            pid,
            tid,
        );
        // unix_stream is dropped here, closing the write end of the socket.
        // This signals EOF to the receiver so it can finish writing the crash report.
    }

    // Wait for the receiver to signal it is done (POLLHUP on the dup'd fd), then reap it.
    // poll_fd is dropped at the end of this function, closing the fd.
    let finish_handle =
        super::process_handle::ProcessHandle::new(poll_fd.as_raw_fd(), receiver_pid);
    finish_handle.finish(&timeout_manager);

    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_test_metadata() -> Metadata {
        Metadata {
            library_name: "test-lib".to_string(),
            library_version: "1.0.0".to_string(),
            family: "test-family".to_string(),
            tags: vec![],
        }
    }

    fn make_test_config() -> CrashtrackerConfiguration {
        CrashtrackerConfiguration::new(
            vec![], // additional_files
            false,  // create_alt_stack
            false,  // use_alt_stack
            None,   // endpoint
            crate::StacktraceCollection::Disabled,
            vec![],                       // signals
            Some(Duration::from_secs(1)), // timeout
            None,                         // unix_socket_path
            false,                        // demangle_names
        )
        .unwrap()
    }

    /// Clears METADATA global, properly freeing any existing Box
    fn clear_metadata() {
        let ptr = METADATA.swap(ptr::null_mut(), SeqCst);
        if !ptr.is_null() {
            unsafe { drop(Box::from_raw(ptr)) };
        }
    }

    /// Clears CONFIG global, properly freeing any existing Box
    fn clear_config() {
        let ptr = CONFIG.swap(ptr::null_mut(), SeqCst);
        if !ptr.is_null() {
            unsafe { drop(Box::from_raw(ptr)) };
        }
    }

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

    #[test]
    fn test_format_message_with_message_and_location() {
        let location = panic::Location::caller();
        let result = format_message("message", "test panic", Some(location));

        assert!(result.starts_with("Process panicked with message \"test panic\" ("));
        assert!(result.contains(&format!("{}:", location.file())));
        assert!(result.contains(&format!(":{}", location.line())));
        assert!(result.ends_with(&format!("{})", location.column())));
    }

    #[test]
    fn test_format_message_with_message_no_location() {
        let result = format_message("message", "test panic", None);
        assert_eq!(result, "Process panicked with message \"test panic\"");
    }

    #[test]
    fn test_format_message_empty_message_with_location() {
        let location = panic::Location::caller();
        let result = format_message("unknown type", "", Some(location));

        assert!(result.starts_with("Process panicked with unknown type ("));
        assert!(result.contains(&format!("{}:", location.file())));
        assert!(result.ends_with(&format!("{})", location.column())));
    }

    #[test]
    fn test_format_message_empty_message_no_location() {
        let result = format_message("unknown type", "", None);
        assert_eq!(result, "Process panicked with unknown type");
    }

    #[test]
    fn test_format_message_different_categories() {
        let result1 = format_message("message", "test", None);
        assert_eq!(result1, "Process panicked with message \"test\"");

        let result2 = format_message("unknown type", "", None);
        assert_eq!(result2, "Process panicked with unknown type");

        let result3 = format_message("custom category", "content", None);
        assert_eq!(result3, "Process panicked with custom category \"content\"");
    }

    #[test]
    fn test_format_message_with_special_characters() {
        let result = format_message("message", "test \"quoted\" 'text'", None);
        assert_eq!(
            result,
            "Process panicked with message \"test \"quoted\" 'text'\""
        );
    }

    // take_metadata_ptr

    #[test]
    fn test_take_metadata_ptr_returns_null_when_unset() {
        clear_metadata();
        assert!(take_metadata_ptr().is_null());
    }

    #[test]
    fn test_take_metadata_ptr_takes_value_and_leaves_null() {
        clear_metadata();
        update_metadata(make_test_metadata()).unwrap();

        let ptr = take_metadata_ptr();
        assert!(!ptr.is_null());

        // Storage is now null; a second take returns null.
        assert!(take_metadata_ptr().is_null());

        // Reconstruct the Box to avoid a leak.
        unsafe { drop(Box::from_raw(ptr)) };
    }

    #[test]
    fn test_take_metadata_ptr_preserves_data() {
        clear_metadata();
        let metadata = make_test_metadata();
        update_metadata(metadata.clone()).unwrap();

        let ptr = take_metadata_ptr();
        assert!(!ptr.is_null());

        let (stored_metadata, stored_json) = unsafe { &*ptr };
        assert_eq!(stored_metadata.library_name, metadata.library_name);
        assert_eq!(stored_metadata.library_version, metadata.library_version);
        assert_eq!(stored_metadata.family, metadata.family);
        // The serialised string must be valid non-empty JSON.
        assert!(!stored_json.is_empty());
        assert!(serde_json::from_str::<serde_json::Value>(stored_json).is_ok());

        unsafe { drop(Box::from_raw(ptr)) };
    }

    // take_config_ptr

    #[test]
    fn test_take_config_ptr_returns_null_when_unset() {
        clear_config();
        assert!(take_config_ptr().is_null());
    }

    #[test]
    fn test_take_config_ptr_takes_value_and_leaves_null() {
        clear_config();
        update_config(make_test_config()).unwrap();

        let ptr = take_config_ptr();
        assert!(!ptr.is_null());

        // Storage is now null; a second take returns null.
        assert!(take_config_ptr().is_null());

        unsafe { drop(Box::from_raw(ptr)) };
    }

    // take_metadata

    #[test]
    fn test_take_metadata_returns_none_when_unset() {
        clear_metadata();
        assert!(take_metadata().is_none());
    }

    #[test]
    fn test_take_metadata_returns_value_and_leaves_none() {
        clear_metadata();
        let metadata = make_test_metadata();
        update_metadata(metadata.clone()).unwrap();

        let (taken_metadata, taken_json) = take_metadata().expect("should return Some");
        assert_eq!(taken_metadata.library_name, metadata.library_name);
        assert_eq!(taken_metadata.library_version, metadata.library_version);
        assert_eq!(taken_metadata.family, metadata.family);
        assert!(!taken_json.is_empty());

        // Second take: storage is empty.
        assert!(take_metadata().is_none());
    }

    // take_config

    #[test]
    fn test_take_config_returns_none_when_unset() {
        clear_config();
        assert!(take_config().is_none());
    }

    #[test]
    fn test_take_config_returns_value_and_leaves_none() {
        clear_config();
        let config = make_test_config();
        update_config(config.clone()).unwrap();

        let (taken_config, taken_json) = take_config().expect("should return Some");
        assert_eq!(taken_config, config);
        assert!(!taken_json.is_empty());
        assert!(serde_json::from_str::<serde_json::Value>(&taken_json).is_ok());

        // Second take: storage is empty.
        assert!(take_config().is_none());
    }
}
