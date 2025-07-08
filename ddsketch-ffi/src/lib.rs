// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

use datadog_ddsketch::DDSketch;
use ddcommon_ffi as ffi;
use std::ptr::NonNull;

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
///
/// # Safety
///
/// The `sketch` parameter must be a valid pointer to uninitialized memory
/// where the DDSketch will be stored.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_new(
    sketch: NonNull<Box<DDSketch>>,
) -> Option<Box<DDSketchError>> {
    let sketch_box = Box::new(DDSketch::default());
    sketch.as_ptr().write(sketch_box);
    None
}

/// Drops a DDSketch instance.
///
/// # Safety
///
/// Only pass null or a pointer to a valid, mutable DDSketch.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_drop(sketch: Option<Box<DDSketch>>) {
    drop(sketch);
}

/// Adds a point to the DDSketch.
///
/// # Safety
///
/// The `sketch` parameter must be a valid pointer to a DDSketch instance.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_add(
    sketch: Option<&mut DDSketch>,
    point: f64,
) -> Option<Box<DDSketchError>> {
    let sketch = match sketch {
        Some(s) => s,
        None => return gen_error!(DDSketchErrorCode::InvalidArgument),
    };

    match sketch.add(point) {
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
/// The `sketch` parameter must be a valid pointer to a DDSketch instance.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_add_with_count(
    sketch: Option<&mut DDSketch>,
    point: f64,
    count: f64,
) -> Option<Box<DDSketchError>> {
    let sketch = match sketch {
        Some(s) => s,
        None => return gen_error!(DDSketchErrorCode::InvalidArgument),
    };

    match sketch.add_with_count(point, count) {
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
/// The `sketch` parameter must be a valid pointer to a DDSketch instance.
/// Returns 0.0 if sketch is null.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_count(sketch: Option<&DDSketch>) -> f64 {
    match sketch {
        Some(s) => s.count(),
        None => 0.0,
    }
}

/// Returns the protobuf-encoded bytes of the DDSketch.
///
/// # Safety
///
/// The `sketch` parameter must be a valid pointer to a DDSketch instance.
/// The returned vector must be freed with `ddog_Vec_U8_drop`.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_encode(sketch: Box<DDSketch>) -> ffi::Vec<u8> {
    let encoded = sketch.encode_to_vec();
    ffi::Vec::from(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;
    use std::ptr::NonNull;

    #[test]
    fn test_ddsketch_new_and_drop() {
        unsafe {
            let mut sketch: MaybeUninit<Box<DDSketch>> = MaybeUninit::uninit();
            let result = ddog_ddsketch_new(NonNull::new(sketch.as_mut_ptr()).unwrap());
            assert!(result.is_none());

            let sketch_box = sketch.assume_init();
            ddog_ddsketch_drop(Some(sketch_box));
        }
    }

    #[test]
    fn test_ddsketch_add() {
        unsafe {
            let mut sketch: MaybeUninit<Box<DDSketch>> = MaybeUninit::uninit();
            ddog_ddsketch_new(NonNull::new(sketch.as_mut_ptr()).unwrap());
            let mut sketch_box = sketch.assume_init();

            let result = ddog_ddsketch_add(Some(&mut sketch_box), 1.0);
            assert!(result.is_none());

            let count = ddog_ddsketch_count(Some(&sketch_box));
            assert_eq!(count, 1.0);

            ddog_ddsketch_drop(Some(sketch_box));
        }
    }

    #[test]
    fn test_ddsketch_add_with_count() {
        unsafe {
            let mut sketch: MaybeUninit<Box<DDSketch>> = MaybeUninit::uninit();
            ddog_ddsketch_new(NonNull::new(sketch.as_mut_ptr()).unwrap());
            let mut sketch_box = sketch.assume_init();

            let result = ddog_ddsketch_add_with_count(Some(&mut sketch_box), 2.0, 3.0);
            assert!(result.is_none());

            let count = ddog_ddsketch_count(Some(&sketch_box));
            assert_eq!(count, 3.0);

            ddog_ddsketch_drop(Some(sketch_box));
        }
    }

    #[test]
    fn test_ddsketch_encode() {
        unsafe {
            let mut sketch: MaybeUninit<Box<DDSketch>> = MaybeUninit::uninit();
            ddog_ddsketch_new(NonNull::new(sketch.as_mut_ptr()).unwrap());
            let mut sketch_box = sketch.assume_init();

            let _ = ddog_ddsketch_add(Some(&mut sketch_box), 1.0);
            let _ = ddog_ddsketch_add(Some(&mut sketch_box), 2.0);

            let encoded = ddog_ddsketch_encode(sketch_box);
            assert!(!encoded.is_empty());

            // Note: In a real implementation, the caller would need to drop the Vec<u8>
            // For now, we'll just let it be consumed by the test
            drop(encoded);
        }
    }
}
