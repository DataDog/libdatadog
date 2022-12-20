// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2022-Present Datadog, Inc.

use crate::{slice::AsBytes, slice::CharSlice};
use ddcommon::metric::Metric;

#[must_use]
#[no_mangle]
pub extern "C" fn ddog_Vec_Metric_new() -> crate::Vec<Metric> {
    crate::Vec::default()
}

#[no_mangle]
pub extern "C" fn ddog_Vec_Metric_drop(_: crate::Vec<Metric>) {}

#[repr(C)]
pub enum PushMetricResult {
    Ok,
    Err(crate::Vec<u8>),
}

#[no_mangle]
pub extern "C" fn ddog_Vec_Metric_PushResult_drop(_: PushMetricResult) {}

/// Creates a new Tag from the provided `key` and `value` by doing a utf8
/// lossy conversion, and pushes into the `vec`. The strings `key` and `value`
/// are cloned to avoid FFI lifetime issues.
///
/// # Safety
/// The `vec` must be a valid reference.
/// The CharSlices `key` and `value` must point to at least many bytes as their
/// `.len` properties claim.
#[no_mangle]
pub unsafe extern "C" fn ddog_Vec_Metric_push(
    vec: &mut crate::Vec<Metric>,
    key: CharSlice,
    value: f32,
) -> PushMetricResult {
    let name = key.to_utf8_lossy().into_owned();
    match Metric::new(name, value) {
        Ok(metric) => {
            vec.push(metric);
            PushMetricResult::Ok
        }
        Err(err) => PushMetricResult::Err(err.as_bytes().to_vec().into()),
    }
}

#[cfg(test)]
mod tests {
    use crate::metrics::*;

    #[test]
    fn empty_metric_name() {
        unsafe {
            let mut metrics = ddog_Vec_Metric_new();
            let result = ddog_Vec_Metric_push(&mut metrics, CharSlice::from(""), 42.);
            assert_eq!(metrics.len(), 0);
            assert!(!matches!(result, PushMetricResult::Ok));
        }
    }

    #[test]
    fn test_get() {
        unsafe {
            let mut metrics = ddog_Vec_Metric_new();
            let metric_name = "my_metric_name";
            let result = ddog_Vec_Metric_push(&mut metrics, CharSlice::from(metric_name), 42.);
            assert!(matches!(result, PushMetricResult::Ok));
            assert_eq!(1, metrics.len());
            let metric = metrics.get(0).unwrap();
            let expected = Metric::new(metric_name.to_string(), 42.).unwrap();
            assert_eq!(metric, &expected);
        }
    }
}
