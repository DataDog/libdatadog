// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_ddsketch::DDSketch;

/// A bin from a DDSketch containing a value and its weight.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DDSketchBin {
    pub value: f64,
    pub weight: f64,
}

/// A vector of DDSketch bins.
#[repr(C)]
pub struct DDSketchBins {
    bins: *mut DDSketchBin,
    len: usize,
    capacity: usize,
}

impl DDSketchBins {
    pub fn new() -> Self {
        Self {
            bins: std::ptr::null_mut(),
            len: 0,
            capacity: 0,
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        if capacity == 0 {
            return Self::new();
        }

        let layout = std::alloc::Layout::array::<DDSketchBin>(capacity).unwrap();
        let bins = unsafe { std::alloc::alloc(layout) as *mut DDSketchBin };

        Self {
            bins,
            len: 0,
            capacity,
        }
    }

    pub fn push(&mut self, bin: DDSketchBin) {
        if self.len == self.capacity {
            self.grow();
        }

        unsafe {
            self.bins.add(self.len).write(bin);
        }
        self.len += 1;
    }

    fn grow(&mut self) {
        let new_capacity = if self.capacity == 0 {
            4
        } else {
            self.capacity * 2
        };
        let new_layout = std::alloc::Layout::array::<DDSketchBin>(new_capacity).unwrap();

        let new_bins = unsafe { std::alloc::alloc(new_layout) as *mut DDSketchBin };

        if !self.bins.is_null() {
            unsafe {
                std::ptr::copy_nonoverlapping(self.bins, new_bins, self.len);
                let old_layout = std::alloc::Layout::array::<DDSketchBin>(self.capacity).unwrap();
                std::alloc::dealloc(self.bins as *mut u8, old_layout);
            }
        }

        self.bins = new_bins;
        self.capacity = new_capacity;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_ptr(&self) -> *const DDSketchBin {
        self.bins
    }
}

impl Drop for DDSketchBins {
    fn drop(&mut self) {
        if !self.bins.is_null() {
            let layout = std::alloc::Layout::array::<DDSketchBin>(self.capacity).unwrap();
            unsafe {
                std::alloc::dealloc(self.bins as *mut u8, layout);
            }
        }
    }
}

/// Returns the ordered bins from the DDSketch.
///
/// # Safety
///
/// The `sketch` parameter must be a valid pointer to a DDSketch instance.
/// The returned bins must be freed with `ddog_ddsketch_bins_drop`.
/// Returns empty bins if sketch is null.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_ordered_bins(sketch: Option<&DDSketch>) -> DDSketchBins {
    let sketch = match sketch {
        Some(s) => s,
        None => return DDSketchBins::new(),
    };

    let bins = sketch.ordered_bins();
    let mut result = DDSketchBins::with_capacity(bins.len());

    for (value, weight) in bins {
        result.push(DDSketchBin { value, weight });
    }

    result
}

/// Drops a DDSketchBins instance.
///
/// # Safety
///
/// Only pass a valid DDSketchBins instance.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_bins_drop(bins: DDSketchBins) {
    drop(bins);
}

/// Returns the length of the DDSketchBins.
///
/// # Safety
///
/// The `bins` parameter must be a valid pointer to a DDSketchBins instance.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_bins_len(bins: &DDSketchBins) -> usize {
    bins.len()
}

/// Returns a pointer to the bins data.
///
/// # Safety
///
/// The `bins` parameter must be a valid pointer to a DDSketchBins instance.
/// The returned pointer is valid until the DDSketchBins is dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_bins_ptr(bins: &DDSketchBins) -> *const DDSketchBin {
    bins.as_ptr()
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

            let ptr = ddog_ddsketch_bins_ptr(&bins);
            assert!(!ptr.is_null());

            ddog_ddsketch_bins_drop(bins);
        }
    }

    #[test]
    fn test_ddsketch_bins_manual() {
        let mut bins = DDSketchBins::new();
        assert_eq!(bins.len(), 0);
        assert!(bins.is_empty());

        bins.push(DDSketchBin {
            value: 1.0,
            weight: 1.0,
        });
        bins.push(DDSketchBin {
            value: 2.0,
            weight: 1.0,
        });

        assert_eq!(bins.len(), 2);
        assert!(!bins.is_empty());

        unsafe {
            let ptr = bins.as_ptr();
            let bin1 = *ptr;
            let bin2 = *ptr.add(1);

            assert_eq!(bin1.value, 1.0);
            assert_eq!(bin1.weight, 1.0);
            assert_eq!(bin2.value, 2.0);
            assert_eq!(bin2.weight, 1.0);
        }
    }
}
