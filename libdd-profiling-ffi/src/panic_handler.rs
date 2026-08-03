// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Global panic handler for the profiling FFI.
//!
//! Callers register a single C callback that fires when `catch_unwind` in
//! `wrap_with_profile_status!` catches a panic. The handler receives the FFI
//! function name and a formatted panic message; the returned `ProfileStatus`
//! uses a static sentinel so the return path does not allocate.

use std::ffi::{c_char, c_void, CString};
use std::sync::atomic::{AtomicPtr, Ordering};

/// C-callable panic handler. Fired once per caught panic in any
/// `ProfileStatus`-returning FFI function.
///
/// # Reentrancy
///
/// The handler MUST NOT call back into any libdatadog profiling FFI function.
/// A panic that occurred mid-mutation may have left handles in a half-mutated
/// state; reentry can compound the corruption.
pub type PanicHandler = extern "C" fn(
    function_name: *const c_char,
    message: *const c_char,
    userdata: *mut c_void,
);

static HANDLER: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());
static USERDATA: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Registers (or clears) the global panic handler. Pass `None` to unregister.
///
/// Thread-safe. The most recent registration wins; concurrent calls are
/// serialized by the atomic stores but can racy-overwrite each other.
///
/// # Safety
///
/// `userdata` must remain valid for as long as the handler may fire, which is
/// "until a subsequent call to this function replaces or clears the handler".
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_set_panic_handler(
    handler: Option<PanicHandler>,
    userdata: *mut c_void,
) {
    let ptr = handler.map_or(std::ptr::null_mut(), |h| h as *mut ());
    HANDLER.store(ptr, Ordering::Release);
    USERDATA.store(userdata, Ordering::Release);
}

/// Fires the registered handler, if any. Best-effort: any failure to format
/// the message is silently dropped, since this is itself the panic path.
pub(crate) fn fire_panic_handler(
    function_name: &str,
    payload: &(dyn std::any::Any + Send),
) {
    let handler_ptr = HANDLER.load(Ordering::Acquire);
    if handler_ptr.is_null() {
        return;
    }
    // SAFETY: `HANDLER` only holds values written via `ddog_prof_set_panic_handler`,
    // which stores a `PanicHandler` fn pointer cast to `*mut ()`.
    let cb: PanicHandler = unsafe { std::mem::transmute(handler_ptr) };

    // Nested catch_unwind so a panic while formatting (e.g. OOM in `format!`)
    // does not unwind through the FFI boundary.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let msg: String = if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else if let Some(s) = payload.downcast_ref::<&'static str>() {
            (*s).to_string()
        } else {
            "<opaque panic payload>".to_string()
        };

        let Ok(fn_c) = CString::new(function_name) else { return };
        let Ok(msg_c) = CString::new(msg) else { return };

        let ud = USERDATA.load(Ordering::Acquire);
        cb(fn_c.as_ptr(), msg_c.as_ptr(), ud);
    }));
}
