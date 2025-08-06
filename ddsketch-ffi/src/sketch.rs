// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_ddsketch::DDSketch;
use ddcommon_ffi as ffi;
use ddcommon_ffi::{Handle, ToInner};

/// A bin from a DDSketch containing a value and its weight.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DDSketchBin {
    pub value: f64,
    pub weight: f64,
}

/// Returns the ordered bins from the DDSketch.
///
/// # Safety
///
/// The `sketch` parameter must be a valid pointer to a DDSketch handle.
/// The returned bins must be freed with `ddog_ddsketch_bins_drop`.
/// Returns empty bins if sketch is null.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_ordered_bins(
    mut sketch: *mut Handle<DDSketch>,
) -> ffi::Vec<DDSketchBin> {
    let sketch_ref = match sketch.to_inner_mut() {
        Ok(s) => s,
        Err(_) => return ffi::Vec::new(),
    };

    let bins = sketch_ref.ordered_bins();
    let result: Vec<DDSketchBin> = bins
        .into_iter()
        .map(|(value, weight)| DDSketchBin { value, weight })
        .collect();

    ffi::Vec::from(result)
}

/// Drops a DDSketchBins instance.
///
/// # Safety
///
/// Only pass a valid DDSketchBins instance.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_bins_drop(bins: ffi::Vec<DDSketchBin>) {
    drop(bins);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ddsketch_bins() {
        use crate::ddog_ddsketch_new;

        unsafe {
            let mut sketch = ddog_ddsketch_new();
            let _ = crate::ddog_ddsketch_add(&mut sketch, 1.0);
            let _ = crate::ddog_ddsketch_add(&mut sketch, 2.0);
            let _ = crate::ddog_ddsketch_add(&mut sketch, 3.0);

            let bins = ddog_ddsketch_ordered_bins(&mut sketch);
            assert!(!bins.is_empty());

            ddog_ddsketch_bins_drop(bins);
            crate::ddog_ddsketch_drop(&mut sketch);
        }
    }

    #[test]
    fn test_ddsketch_bins_manual() {
        let bins_vec = vec![
            DDSketchBin {
                value: 1.0,
                weight: 1.0,
            },
            DDSketchBin {
                value: 2.0,
                weight: 1.0,
            },
        ];

        let bins = ffi::Vec::from(bins_vec);
        assert_eq!(bins.len(), 2);
        assert!(!bins.is_empty());

        // Test that we can access the data through the slice
        let slice = bins.as_slice();
        assert_eq!(slice[0].value, 1.0);
        assert_eq!(slice[0].weight, 1.0);
        assert_eq!(slice[1].value, 2.0);
        assert_eq!(slice[1].weight, 1.0);

        unsafe {
            ddog_ddsketch_bins_drop(bins);
        }
    }
}
