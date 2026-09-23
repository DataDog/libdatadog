// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::RemoteConfigNotification;
use datadog_sidecar::windows::remote_config_notification::RemoteConfigNotification as Notification;
use libdd_common_ffi::{self as ffi, MaybeError};
use libdd_telemetry_ffi::try_c;
use std::ffi::c_void;

// Release artifacts use panic=abort, so these FFI boundaries need no unwind guards.

/// Create a Windows notification that invokes `callback(context)` when remote configuration may
/// have changed.
///
/// On success, `*out` receives a newly allocated notification. Pass that pointer to
/// `ddog_sidecar_session_set_config` to associate it with a session, and eventually release it
/// with `ddog_sidecar_remote_config_notification_drop`. Session configuration does not take
/// ownership of the notification.
///
/// The callback runs asynchronously on a Windows thread-pool thread. Calls for the same
/// notification do not overlap, but several remote configuration updates may be represented by
/// one call. Treat the callback as a prompt to read the latest configuration rather than as a
/// count of updates.
///
/// If the function returns an error, a valid `out` parameter is set to NULL.
///
/// # Safety
///
/// - `out` must point to writable storage for one notification pointer.
/// - `callback` must be non-NULL and safe to call with `context` from a Windows thread-pool thread.
/// - If creation succeeds, the callback code and any data reached through `context` must remain
///   valid until `ddog_sidecar_remote_config_notification_drop` returns.
/// - The callback must not drop its own notification.
#[no_mangle]
pub unsafe extern "C" fn ddog_sidecar_remote_config_notification_new(
    callback: Option<unsafe extern "C" fn(*mut c_void)>,
    context: *mut c_void,
    out: *mut *mut RemoteConfigNotification,
) -> MaybeError {
    let out = try_c!(out.as_mut().ok_or("notification output is null"));
    *out = std::ptr::null_mut();
    let callback = try_c!(callback.ok_or("notification callback is null"));
    let inner = try_c!(Notification::new(callback, context));
    *out = Box::into_raw(Box::new(RemoteConfigNotification { inner }));
    MaybeError::None
}

/// Disable a remote configuration notification and release it.
///
/// Passing NULL has no effect. If its callback is currently running, this function waits for the
/// callback to return. Once this function returns, no callback for this notification is running or
/// can start, so the caller may safely release the callback context or unload the callback code.
/// A sidecar that still has the session configuration may continue sending signals, but those
/// signals can no longer invoke the callback.
///
/// # Safety
///
/// - `notification` must be NULL or a live pointer returned by
///   `ddog_sidecar_remote_config_notification_new`.
/// - A non-NULL pointer may be passed to this function only once and must not be used concurrently
///   by another call, including `ddog_sidecar_session_set_config`.
/// - This function must not be called from the notification's callback.
#[no_mangle]
pub unsafe extern "C" fn ddog_sidecar_remote_config_notification_drop(
    notification: *mut RemoteConfigNotification,
) {
    if !notification.is_null() {
        drop(Box::from_raw(notification));
    }
}
