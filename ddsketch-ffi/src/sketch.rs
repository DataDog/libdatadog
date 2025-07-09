// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_ddsketch::DDSketch;
use ddcommon_ffi as ffi;

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
/// The `sketch` parameter must be a valid pointer to a DDSketch instance.
/// The returned bins must be freed with `ddog_ddsketch_bins_drop`.
/// Returns empty bins if sketch is null.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_ordered_bins(
    sketch: Option<&DDSketch>,
) -> ffi::Vec<DDSketchBin> {
    let sketch = match sketch {
        Some(s) => s,
        None => return ffi::Vec::new(),
    };

    let bins = sketch.ordered_bins();
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
    use datadog_ddsketch::DDSketch;

    #[test]
    fn test_ddsketch_bins() {
        let mut sketch = DDSketch::default();
        sketch.add(1.0).unwrap();
        sketch.add(2.0).unwrap();
        sketch.add(3.0).unwrap();

        unsafe {
            let bins = ddog_ddsketch_ordered_bins(Some(&sketch));
            assert!(bins.len() > 0);

            ddog_ddsketch_bins_drop(bins);
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
