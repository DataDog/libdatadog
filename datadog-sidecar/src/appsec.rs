// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{c_char, CString};
use std::sync::atomic::{AtomicPtr, Ordering};
use tracing::{error, info};

pub type OnMessageFn = extern "C" fn(*const c_char, usize, u64, *const u8, usize) -> AppsecCResponse;
pub type OnDisconnectFn = extern "C" fn(*const c_char, usize);
pub type FreeResponseFn = extern "C" fn(*mut u8, usize, usize);

/// Starts the AppSec helper if the sidecar configuration requests it.
///
/// Returns `true` if the helper started successfully and `shutdown` should be
/// called later; `false` otherwise (disabled or failed to start).
pub fn maybe_start(_appsec_config: &crate::config::AppSecConfig) -> bool {
    info!("Starting appsec helper");
    #[allow(clippy::unwrap_used)]
    let sym = CString::new("appsec_helper_main").unwrap();
    let func_ptr = unsafe { libc::dlsym(libc::RTLD_DEFAULT, sym.as_ptr()) };
    if func_ptr.is_null() {
        error!("Failed to load appsec helper: can't find the symbol 'appsec_helper_main'");
        return false;
    }

    let entry_fn: extern "C" fn() -> i32 = unsafe { std::mem::transmute(func_ptr) };
    if entry_fn() != 0 {
        error!("Appsec helper failed to start");
        return false;
    }

    info!("Appsec helper started");
    true
}

/// Shuts down the AppSec helper via its exported symbol.
pub fn shutdown() {
    info!("Shutting down appsec helper");
    #[allow(clippy::unwrap_used)]
    let sym = CString::new("appsec_helper_shutdown").unwrap();
    let func_ptr = unsafe { libc::dlsym(libc::RTLD_DEFAULT, sym.as_ptr()) };
    if func_ptr.is_null() {
        error!("Failed to shut down appsec helper: can't find the symbol 'appsec_helper_shutdown'");
        return;
    }

    let shutdown_fn: extern "C" fn() -> i32 = unsafe { std::mem::transmute(func_ptr) };
    if shutdown_fn() != 0 {
        error!("Appsec helper failed to shutdown cleanly");
        return;
    }

    info!("Appsec helper shutdown");
}

/// Raw response returned by the AppSec `on_message` C callback.
///
/// When `ptr` is non-null the buffer must be freed by calling the `free_response`
/// function registered via `ddog_appsec_register_message_handler`, which uses
/// the allocator that created the buffer (helper-rust's allocator).
#[repr(C)]
pub struct AppsecCResponse {
    pub ptr: *mut u8,
    pub len: usize,
    pub capacity: usize,
    /// If true, the extension session should be disconnected after this response.
    pub disconnect: bool,
}

/// Owned response from `dispatch_message`.
///
/// Frees the buffer through the registered `free_response` callback on drop, so
/// the correct (helper-rust) allocator is used regardless of what allocator the
/// sidecar itself uses.
pub struct AppsecResponse {
    ptr: *mut u8,
    len: usize,
    capacity: usize,
    pub disconnect: bool,
}

impl AppsecResponse {
    pub fn as_bytes(&self) -> &[u8] {
        if self.ptr.is_null() {
            &[]
        } else {
            // SAFETY: ptr/len are valid for the lifetime of self.
            unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
        }
    }
}

impl Drop for AppsecResponse {
    fn drop(&mut self) {
        if self.ptr.is_null() {
            return;
        }
        let free_ptr = FREE_RESPONSE.load(Ordering::Acquire);
        if free_ptr.is_null() {
            // Should not happen: free_response is always set alongside on_message.
            error!("AppSec response buffer leaked: free_response not registered");
            return;
        }
        // SAFETY: fn_ptr was stored by ddog_appsec_register_message_handler and is
        // a valid FreeResponseFn for the lifetime of the process.
        let free_fn: FreeResponseFn = unsafe { std::mem::transmute(free_ptr) };
        free_fn(self.ptr, self.len, self.capacity);
    }
}

// SAFETY: ownership of the buffer is held by AppsecResponse; only one thread
// accesses it at a time.
unsafe impl Send for AppsecResponse {}

static ON_MESSAGE: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());
static ON_DISCONNECT: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());
static FREE_RESPONSE: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

/// Called by the AppSec helper at startup to register its message-handling callbacks.
///
/// The C-exported entry point is `ddog_appsec_register_message_handler`, declared in
/// `datadog-sidecar-ffi` so that it appears in the generated header.
///
/// # Safety
/// - All function pointers must remain valid for the lifetime of the process.
/// - `on_message` and `on_disconnect` may be called from a `spawn_blocking` thread
///   and are allowed to block.
/// - `free_response(ptr, len, capacity)` will be called to free any non-null buffer
///   returned by `on_message`; it must use the same allocator that produced the buffer.
pub fn register_message_handler(
    on_message: OnMessageFn,
    on_disconnect: OnDisconnectFn,
    free_response: FreeResponseFn,
) {
    ON_MESSAGE.store(on_message as *mut _, Ordering::Release);
    ON_DISCONNECT.store(on_disconnect as *mut _, Ordering::Release);
    FREE_RESPONSE.store(free_response as *mut _, Ordering::Release);
}

/// Dispatches an AppSec helper message to the registered callback.
///
/// Returns `Some(response)` when a handler is registered, or `None` if no handler
/// has been registered yet. The response buffer is freed via `free_response` when
/// the returned `AppsecResponse` is dropped.
pub fn dispatch_message(session_id: &str, thread_id: u64, data: &[u8]) -> Option<AppsecResponse> {
    let on_message_ptr = ON_MESSAGE.load(Ordering::Acquire);
    if on_message_ptr.is_null() {
        return None;
    }

    // SAFETY: fn_ptr was stored by ddog_appsec_register_message_handler and is a valid
    // OnMessageFn for the lifetime of the process.
    let on_message: OnMessageFn = unsafe { std::mem::transmute(on_message_ptr) };

    let resp = on_message(
        session_id.as_ptr() as *const c_char,
        session_id.len(),
        thread_id,
        data.as_ptr(),
        data.len(),
    );

    Some(AppsecResponse {
        ptr: resp.ptr,
        len: resp.len,
        capacity: resp.capacity,
        disconnect: resp.disconnect,
    })
}

/// Dispatches a session-disconnect notification to the registered callback.
pub fn dispatch_disconnect(session_id: &str) {
    let on_disconnect_ptr = ON_DISCONNECT.load(Ordering::Acquire);
    if on_disconnect_ptr.is_null() {
        return;
    }
    // SAFETY: fn_ptr was stored by ddog_appsec_register_message_handler and is a valid
    // OnDisconnectFn for the lifetime of the process.
    let on_disconnect: OnDisconnectFn = unsafe { std::mem::transmute(on_disconnect_ptr) };

    on_disconnect(session_id.as_ptr() as *const c_char, session_id.len());
}
