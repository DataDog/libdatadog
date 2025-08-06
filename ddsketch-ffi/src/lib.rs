// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

use datadog_ddsketch::DDSketch;
use ddcommon_ffi as ffi;
use ddcommon_ffi::{Handle, ToInner};

mod error;
mod sketch;

pub use error::*;
pub use sketch::*;

macro_rules! gen_error {
    ($l:expr) => {
        Some(Box::new(DDSketchError::new($l, &$l.to_string())))
    };
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
) -> Option<Box<DDSketchError>> {
    let sketch_ref = match sketch.to_inner_mut() {
        Ok(s) => s,
        Err(_) => return gen_error!(DDSketchErrorCode::InvalidArgument),
    };

    match sketch_ref.add(point) {
        Ok(_) => None,
        Err(e) => Some(Box::new(DDSketchError::new(
            DDSketchErrorCode::InvalidInput,
            &e.to_string(),
        ))),
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
) -> Option<Box<DDSketchError>> {
    let sketch_ref = match sketch.to_inner_mut() {
        Ok(s) => s,
        Err(_) => return gen_error!(DDSketchErrorCode::InvalidArgument),
    };

    match sketch_ref.add_with_count(point, count) {
        Ok(_) => None,
        Err(e) => Some(Box::new(DDSketchError::new(
            DDSketchErrorCode::InvalidInput,
            &e.to_string(),
        ))),
    }
}

/// Returns the count of points in the DDSketch.
///
/// # Safety
///
/// The `sketch` parameter must be a valid pointer to a DDSketch handle.
/// Returns 0.0 if sketch is null.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_count(mut sketch: *mut Handle<DDSketch>) -> f64 {
    match sketch.to_inner_mut() {
        Ok(s) => s.count(),
        Err(_) => 0.0,
    }
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
            assert!(result.is_none());

            let count = ddog_ddsketch_count(&mut sketch);
            assert_eq!(count, 1.0);

            ddog_ddsketch_drop(&mut sketch);
        }
    }

    #[test]
    fn test_ddsketch_add_with_count() {
        unsafe {
            let mut sketch = ddog_ddsketch_new();

            let result = ddog_ddsketch_add_with_count(&mut sketch, 2.0, 3.0);
            assert!(result.is_none());

            let count = ddog_ddsketch_count(&mut sketch);
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
            drop(encoded);
        }
    }
}
