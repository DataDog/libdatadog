// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

use ddcommon_ffi as ffi;
use ddcommon_ffi::{Error, Handle, ToInner, VoidResult};
use libdd_ddsketch::DDSketch;
use std::mem::MaybeUninit;

const NULL_POINTER_ERROR: &str = "null pointer provided";

fn ddsketch_error(msg: &str) -> Error {
    Error::from(msg)
}

/// Creates a new DDSketch instance with default configuration.
#[no_mangle]
pub extern "C" fn ddog_ddsketch_new() -> Handle<DDSketch> {
    DDSketch::default().into()
}

/// Drops a DDSketch instance.
///
/// # Safety
///
/// The sketch handle must have been created by this library and not already dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_drop(mut sketch: *mut Handle<DDSketch>) {
    drop(sketch.take());
}

/// Adds a point to the DDSketch.
///
/// # Safety
///
/// The `sketch` parameter must be a valid pointer to a DDSketch handle.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_add(
    mut sketch: *mut Handle<DDSketch>,
    point: f64,
) -> VoidResult {
    let sketch_ref = match sketch.to_inner_mut() {
        Ok(s) => s,
        Err(e) => return VoidResult::Err(ddsketch_error(&e.to_string())),
    };

    match sketch_ref.add(point) {
        Ok(_) => VoidResult::Ok,
        Err(e) => VoidResult::Err(ddsketch_error(&e.to_string())),
    }
}

/// Adds a point with a specific count to the DDSketch.
///
/// # Safety
///
/// The `sketch` parameter must be a valid pointer to a DDSketch handle.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_add_with_count(
    mut sketch: *mut Handle<DDSketch>,
    point: f64,
    count: f64,
) -> VoidResult {
    let sketch_ref = match sketch.to_inner_mut() {
        Ok(s) => s,
        Err(e) => return VoidResult::Err(ddsketch_error(&e.to_string())),
    };

    match sketch_ref.add_with_count(point, count) {
        Ok(_) => VoidResult::Ok,
        Err(e) => VoidResult::Err(ddsketch_error(&e.to_string())),
    }
}

/// Returns the count of points in the DDSketch via the output parameter.
///
/// # Safety
///
/// The `sketch` parameter must be a valid pointer to a DDSketch handle.
/// The `count_out` parameter must be a valid pointer to uninitialized f64 memory.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_count(
    mut sketch: *mut Handle<DDSketch>,
    count_out: *mut MaybeUninit<f64>,
) -> VoidResult {
    if count_out.is_null() {
        return VoidResult::Err(ddsketch_error(NULL_POINTER_ERROR));
    }

    let sketch_ref = match sketch.to_inner_mut() {
        Ok(s) => s,
        Err(e) => return VoidResult::Err(ddsketch_error(&e.to_string())),
    };

    count_out.write(MaybeUninit::new(sketch_ref.count()));
    VoidResult::Ok
}

/// Returns the protobuf-encoded bytes of the DDSketch.
/// The sketch handle is consumed by this operation.
///
/// # Safety
///
/// The `sketch` parameter must be a valid pointer to a DDSketch handle.
/// The returned vector must be freed with `ddog_Vec_U8_drop`.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_encode(mut sketch: *mut Handle<DDSketch>) -> ffi::Vec<u8> {
    match sketch.take() {
        Ok(ddsketch) => {
            let encoded = ddsketch.encode_to_vec();
            ffi::Vec::from(encoded)
        }
        Err(_) => ffi::Vec::new(),
    }
}

/// Frees the memory allocated for a Vec<u8> returned by ddsketch functions.
///
/// # Safety
///
/// The vec parameter must be a valid Vec<u8> returned by this library.
/// After being called, the vec will not point to valid memory.
#[no_mangle]
pub extern "C" fn ddog_Vec_U8_drop(_vec: ffi::Vec<u8>) {
    // The Vec will be automatically dropped when it goes out of scope
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ddsketch_new_and_drop() {
        unsafe {
            let mut sketch = ddog_ddsketch_new();
            ddog_ddsketch_drop(&mut sketch);
        }
    }

    #[test]
    fn test_ddsketch_add() {
        unsafe {
            let mut sketch = ddog_ddsketch_new();

            let result = ddog_ddsketch_add(&mut sketch, 1.0);
            assert!(matches!(result, VoidResult::Ok));

            let mut count = MaybeUninit::uninit();
            let result = ddog_ddsketch_count(&mut sketch, &mut count);
            assert!(matches!(result, VoidResult::Ok));
            let count = count.assume_init();
            assert_eq!(count, 1.0);

            ddog_ddsketch_drop(&mut sketch);
        }
    }

    #[test]
    fn test_ddsketch_add_with_count() {
        unsafe {
            let mut sketch = ddog_ddsketch_new();

            let result = ddog_ddsketch_add_with_count(&mut sketch, 2.0, 3.0);
            assert!(matches!(result, VoidResult::Ok));

            let mut count = MaybeUninit::uninit();
            let result = ddog_ddsketch_count(&mut sketch, &mut count);
            assert!(matches!(result, VoidResult::Ok));
            let count = count.assume_init();
            assert_eq!(count, 3.0);

            ddog_ddsketch_drop(&mut sketch);
        }
    }

    #[test]
    fn test_ddsketch_encode() {
        unsafe {
            let mut sketch = ddog_ddsketch_new();

            let _ = ddog_ddsketch_add(&mut sketch, 1.0);
            let _ = ddog_ddsketch_add(&mut sketch, 2.0);

            let encoded = ddog_ddsketch_encode(&mut sketch);
            assert!(!encoded.is_empty());

            // sketch is consumed by encode, so no need to drop it
            // Clean up the encoded Vec
            ddog_Vec_U8_drop(encoded);
        }
    }

    #[test]
    fn test_error_messages() {
        unsafe {
            let mut sketch = ddog_ddsketch_new();

            // invalid point
            let result = ddog_ddsketch_add(&mut sketch, -1.0);
            match result {
                VoidResult::Err(err) => {
                    let msg = err.as_ref();
                    assert!(msg.contains("point is invalid"));
                }
                VoidResult::Ok => panic!("Expected error for negative point"),
            }

            // invalid count
            let result = ddog_ddsketch_add_with_count(&mut sketch, 1.0, f64::NAN);
            match result {
                VoidResult::Err(err) => {
                    let msg = err.as_ref();
                    assert!(msg.contains("count is invalid"));
                }
                VoidResult::Ok => panic!("Expected error for NaN count"),
            }

            ddog_ddsketch_drop(&mut sketch);
        }
    }
}
