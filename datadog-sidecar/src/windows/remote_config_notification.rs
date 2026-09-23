// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Windows client-owned remote configuration notifications.

use crate::service::RemoteConfigNotifyTarget;
use libdd_ipc::platform::PlatformHandle;
use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr;
use std::sync::{Arc, Mutex};
use winapi::shared::minwindef::TRUE;
use winapi::um::synchapi::CreateEventW;
use winapi::um::threadpoolapiset::{
    CloseThreadpoolWait, CreateThreadpoolWait, SetThreadpoolWait, WaitForThreadpoolWaitCallbacks,
};
use winapi::um::winnt::{PTP_CALLBACK_INSTANCE, PTP_WAIT, TP_WAIT_RESULT};

/// Owns a wait registered on the process's shared Windows thread pool.
///
/// Drop this object before destroying callback state or unloading callback code. The callback
/// must not drop its own notification.
pub struct RemoteConfigNotification {
    state: Box<CallbackState>,
    wait: PTP_WAIT,
}

impl RemoteConfigNotification {
    /// Register a callback on an unnamed auto-reset event.
    ///
    /// # Safety
    /// `callback` and `context` must remain valid and safe to invoke from a Windows thread-pool
    /// thread until drop returns. The callback must obey the type's documented restrictions.
    pub unsafe fn new(
        callback: unsafe extern "C" fn(*mut c_void),
        context: *mut c_void,
    ) -> io::Result<Self> {
        let event = CreateEventW(ptr::null_mut(), 0, 0, ptr::null());
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut state = Box::new(CallbackState {
            event: PlatformHandle::from_raw_handle(event.cast()),
            id: rand::random(),
            callback,
            context,
            enabled: Arc::new(Mutex::new(true)),
        });
        let wait = CreateThreadpoolWait(
            Some(notify),
            ptr::from_mut(&mut *state).cast(),
            ptr::null_mut(),
        );
        if wait.is_null() {
            return Err(io::Error::last_os_error());
        }
        SetThreadpoolWait(wait, state.event.as_raw_handle().cast(), ptr::null_mut());
        Ok(Self { state, wait })
    }

    /// Return an event reference for IPC handle transfer. Clones retain a stable identity.
    pub fn target(&self) -> RemoteConfigNotifyTarget {
        RemoteConfigNotifyTarget {
            event: self.state.event.clone(),
            id: self.state.id,
        }
    }
}

impl Drop for RemoteConfigNotification {
    fn drop(&mut self) {
        {
            let mut enabled = self.state.enabled.lock().unwrap_or_else(|e| e.into_inner());
            *enabled = false;
            // The callback holds this mutex while deciding whether to rearm the wait. Disarming
            // it before releasing the mutex prevents the callback from rearming it after shutdown
            // has begun.
            unsafe { SetThreadpoolWait(self.wait, ptr::null_mut(), ptr::null_mut()) };
        }
        // A callback already in progress may need to acquire `enabled` before it can finish, so
        // release the mutex before waiting for active callbacks to drain.
        unsafe {
            // cancel callbacks that have not started and wait for any running callback to finish
            WaitForThreadpoolWaitCallbacks(self.wait, TRUE);
            CloseThreadpoolWait(self.wait);
        }
    }
}

struct CallbackState {
    event: PlatformHandle<OwnedHandle>,
    id: u128,
    callback: unsafe extern "C" fn(*mut c_void),
    context: *mut c_void,
    enabled: Arc<Mutex<bool>>,
}

unsafe extern "system" fn notify(
    _instance: PTP_CALLBACK_INSTANCE,
    context: *mut winapi::ctypes::c_void,
    wait: PTP_WAIT,
    _result: TP_WAIT_RESULT,
) {
    // Release artifacts use panic=abort, so this system callback needs no unwind guard.
    let state = &*context.cast::<CallbackState>();
    if !*state.enabled.lock().unwrap_or_else(|e| e.into_inner()) {
        return;
    }
    (state.callback)(state.context);
    let enabled = state.enabled.lock().unwrap_or_else(|e| e.into_inner());
    if *enabled {
        // Waits are one-shot. Rearming only after client work coalesces concurrent signals.
        SetThreadpoolWait(wait, state.event.as_raw_handle().cast(), ptr::null_mut());
    }
}
