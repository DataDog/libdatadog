// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::api::{self, Registry};
use std::ptr;
use std::sync::Mutex;

#[derive(Default)]
pub struct Reactor(Mutex<Registry>);

macro_rules! catch_panic {
    ($failure:expr, $operation:expr) => {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe($operation)) {
            Ok(Some(value)) => value,
            Ok(None) => $failure,
            Err(payload) => {
                std::mem::forget(payload);
                $failure
            }
        }
    };
}

pub(crate) use catch_panic;

pub(crate) unsafe fn with_registry<Value>(
    reactor: *mut Reactor,
    operation: impl FnOnce(&mut Registry) -> Value,
) -> Option<Value> {
    // SAFETY: The entry point requires a live, aligned handle for the full call.
    let reactor = unsafe { reactor.as_ref() }?;
    let mut registry = reactor.0.lock().ok()?;
    Some(operation(&mut registry))
}

// allow(ffi-panic-boundary): returns a compile-time constant.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_rc_version() -> i32 {
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_rc_reactor_new() -> *mut Reactor {
    catch_panic!(ptr::null_mut(), || {
        Some(Box::into_raw(Box::<Reactor>::default()))
    })
}

/// # Safety
/// A non-null handle must come from `ddog_rc_reactor_new` and have no active calls or borrows.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_rc_reactor_drop(reactor: *mut Reactor) {
    if !reactor.is_null() {
        catch_panic!((), || {
            // SAFETY: The caller transfers sole ownership of the original allocation.
            drop(unsafe { Box::from_raw(reactor) });
            Some(())
        });
    }
}

/// # Safety
/// The reactor must be live. Read at most `ddog_rc_buffer_len` bytes and end the borrow before
/// another call on this reactor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_rc_buffer_bytes(reactor: *mut Reactor, buffer: i32) -> *const u8 {
    // SAFETY: The caller preserves the handle and observes the returned borrow lifetime.
    catch_panic!(ptr::null(), || unsafe {
        with_registry(reactor, |registry| api::read_bytes(registry, buffer))
    })
}

/// # Safety
/// The reactor must be live and no other call or borrow may overlap the writable borrow.
/// Write at most `ddog_rc_buffer_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_rc_input_bytes(reactor: *mut Reactor, buffer: i32) -> *mut u8 {
    // SAFETY: The caller preserves the handle and has exclusive access until the write ends.
    catch_panic!(ptr::null_mut(), || unsafe {
        with_registry(reactor, |registry| api::write_bytes(registry, buffer))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unwind_fails_the_call_and_poisons_the_registry() {
        let mut reactor = Reactor::default();
        let handle = &raw mut reactor;
        let result: i32 = catch_panic!(-1, || unsafe {
            with_registry(handle, |_| std::panic::resume_unwind(Box::new(())))
        });
        assert_eq!(result, -1);
        assert_eq!(unsafe { crate::ddog_rc_alloc(handle, 1) }, -1);
        assert!(unsafe { ddog_rc_buffer_bytes(handle, 1) }.is_null());
        assert!(unsafe { ddog_rc_input_bytes(handle, 1) }.is_null());
    }
}
