// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ffi::MaybeError;
use ffi::slice::AsBytes;
use function_name::named;
use libdd_capabilities_impl::NativeCapabilities;
use libdd_common::tag::Tag;
use libdd_common_ffi as ffi;
use libdd_telemetry::{
    data::{
        Configuration, ConfigurationOrigin, Endpoint, Method,
        metrics::{MetricNamespace, MetricType},
    },
    metrics::ContextKey,
    worker::{LifecycleAction, TelemetryActions},
};

/// FFI-facing alias: the C ABI surface is native-only, so the worker handle is
/// always pinned to [`NativeCapabilities`].
fn panic_to_error(_: Box<dyn std::any::Any + Send>) -> MaybeError {
    MaybeError::Some(ffi::Error::from("telemetry FFI call panicked"))
}

type TelemetryWorkerHandle = libdd_telemetry::worker::TelemetryWorkerHandle<NativeCapabilities>;

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_telemetry_handle_add_dependency(
    handle: &TelemetryWorkerHandle,
    dependency_name: ffi::CharSlice,
    dependency_version: ffi::CharSlice,
) -> MaybeError {
    let name = crate::try_c!(dependency_name.try_to_string());
    let version = crate::try_c!(dependency_version.try_to_string_option());
    crate::try_c!(handle.add_dependency(name, version, None));
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_telemetry_handle_add_integration(
    handle: &TelemetryWorkerHandle,
    dependency_name: ffi::CharSlice,
    dependency_version: ffi::CharSlice,
    enabled: bool,
    compatible: ffi::Option<bool>,
    auto_enabled: ffi::Option<bool>,
) -> MaybeError {
    let name = crate::try_c!(dependency_name.try_to_string());
    let version = crate::try_c!(dependency_version.try_to_string_option());
    crate::try_c!(handle.add_integration(
        name,
        enabled,
        version,
        compatible.into(),
        auto_enabled.into(),
        None,
    ));
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
#[named]
/// * indentifier: identifies a logging location uniquely. This can for instance be the template
///   using for the log message or the concatenated file + line of the origin of the log
/// * stack_trace: stack trace associated with the log. If no stack trace is available, an empty
///   string should be passed
pub unsafe extern "C" fn ddog_telemetry_handle_add_log(
    handle: &TelemetryWorkerHandle,
    indentifier: ffi::CharSlice,
    message: ffi::CharSlice,
    level: libdd_telemetry::data::LogLevel,
    stack_trace: ffi::CharSlice,
) -> MaybeError {
    let id = crate::try_c!(indentifier.try_as_bytes().map_err(|e| {
        let func = function_name!();
        format!("{func} failed: identifier failed to convert to a byte slice: {e}")
    }));
    crate::try_c!(handle.add_log(
        id,
        crate::try_c!(message.try_to_string()),
        level,
        crate::try_c!(stack_trace.try_to_string_option()),
    ));
    MaybeError::None
}

/// * value: `None` is reported as a `null` value; `Some("")` as an empty string
/// * config_id: an empty slice means no config id
/// * seq_id: optional sequence id of the configuration entry
#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_telemetry_handle_add_config(
    handle: &TelemetryWorkerHandle,
    name: ffi::CharSlice,
    value: ffi::Option<ffi::CharSlice>,
    origin: ConfigurationOrigin,
    config_id: ffi::CharSlice,
    seq_id: ffi::Option<u64>,
) -> MaybeError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let config = Configuration {
            name: crate::try_c!(name.try_to_string()),
            value: match Option::<ffi::CharSlice>::from(value) {
                Some(v) => Some(crate::try_c!(v.try_to_string())),
                None => None,
            },
            origin,
            config_id: crate::try_c!(config_id.try_to_string_option()),
            seq_id: seq_id.into(),
        };
        crate::try_c!(handle.try_send_msg(TelemetryActions::AddConfig(config)));
        MaybeError::None
    }))
    .unwrap_or_else(panic_to_error)
}

/// * version: an empty slice means no version
#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_telemetry_handle_add_product_change(
    handle: &TelemetryWorkerHandle,
    product: ffi::CharSlice,
    enabled: bool,
    version: ffi::CharSlice,
) -> MaybeError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let product = crate::try_c!(product.try_to_string());
        let version = crate::try_c!(version.try_to_string_option());
        crate::try_c!(handle.add_product_change(product, enabled, version));
        MaybeError::None
    }))
    .unwrap_or_else(panic_to_error)
}

/// * path: an empty slice means no path
/// * request_body_type, response_body_type, response_code: an empty slice means not set
#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_telemetry_handle_add_endpoint(
    handle: &TelemetryWorkerHandle,
    method: ffi::Option<Method>,
    path: ffi::CharSlice,
    operation_name: ffi::CharSlice,
    resource_name: ffi::CharSlice,
    request_body_type: ffi::Slice<ffi::CharSlice>,
    response_body_type: ffi::Slice<ffi::CharSlice>,
    response_code: ffi::Slice<u32>,
) -> MaybeError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fn strings(s: ffi::Slice<ffi::CharSlice>) -> Result<Option<Vec<String>>, String> {
            if s.is_empty() {
                return Ok(None);
            }
            s.as_slice()
                .iter()
                .map(|c| c.try_to_string().map_err(|e| e.to_string()))
                .collect::<Result<Vec<_>, _>>()
                .map(Some)
        }
        let codes = response_code.as_slice();
        let endpoint = Endpoint {
            method: method.into(),
            path: crate::try_c!(path.try_to_string_option()),
            operation_name: crate::try_c!(operation_name.try_to_string()),
            resource_name: crate::try_c!(resource_name.try_to_string()),
            request_body_type: crate::try_c!(strings(request_body_type)),
            response_body_type: crate::try_c!(strings(response_body_type)),
            response_code: (!codes.is_empty()).then(|| codes.to_vec()),
        };
        crate::try_c!(handle.try_send_msg(TelemetryActions::AddEndpoint(endpoint)));
        MaybeError::None
    }))
    .unwrap_or_else(panic_to_error)
}

/// Flushes aggregated metrics into the pending payload queue
#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_telemetry_handle_flush_metrics(
    handle: &TelemetryWorkerHandle,
) -> MaybeError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::try_c!(handle.try_send_msg(TelemetryActions::Lifecycle(
            LifecycleAction::FlushMetricAggr
        )));
        MaybeError::None
    }))
    .unwrap_or_else(panic_to_error)
}

/// Sends all pending telemetry data
#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_telemetry_handle_flush_data(
    handle: &TelemetryWorkerHandle,
) -> MaybeError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::try_c!(handle.try_send_msg(TelemetryActions::Lifecycle(LifecycleAction::FlushData)));
        MaybeError::None
    }))
    .unwrap_or_else(panic_to_error)
}

/// Sends an app-extended-heartbeat message
#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_telemetry_handle_send_extended_heartbeat(
    handle: &TelemetryWorkerHandle,
) -> MaybeError {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::try_c!(handle.try_send_msg(TelemetryActions::Lifecycle(
            LifecycleAction::ExtendedHeartbeat
        )));
        MaybeError::None
    }))
    .unwrap_or_else(panic_to_error)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_telemetry_handle_start(handle: &TelemetryWorkerHandle) -> MaybeError {
    crate::try_c!(handle.send_start());
    MaybeError::None
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_telemetry_handle_clone(
    handle: &TelemetryWorkerHandle,
) -> Box<TelemetryWorkerHandle> {
    Box::new(handle.clone())
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_telemetry_handle_stop(handle: &TelemetryWorkerHandle) -> MaybeError {
    crate::try_c!(handle.send_stop());
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
/// * compatible: should be false if the metric is language specific, true otherwise
pub unsafe extern "C" fn ddog_telemetry_handle_register_metric_context(
    handle: &TelemetryWorkerHandle,
    name: ffi::CharSlice,
    metric_type: MetricType,
    tags: ffi::Vec<Tag>,
    common: bool,
    namespace: MetricNamespace,
) -> ContextKey {
    handle.register_metric_context(
        name.to_utf8_lossy().into_owned(),
        tags.into(),
        metric_type,
        common,
        namespace,
    )
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_telemetry_handle_add_point(
    handle: &TelemetryWorkerHandle,
    context_key: &ContextKey,
    value: f64,
) -> MaybeError {
    crate::try_c!(handle.add_point(value, context_key, Vec::new()));
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_telemetry_handle_add_point_with_tags(
    handle: &TelemetryWorkerHandle,
    context_key: &ContextKey,
    value: f64,
    extra_tags: ffi::Vec<Tag>,
) -> MaybeError {
    crate::try_c!(handle.add_point(value, context_key, extra_tags.into()));
    MaybeError::None
}

#[unsafe(no_mangle)]
/// This function takes ownership of the handle. It should not be used after calling it
pub extern "C" fn ddog_telemetry_handle_wait_for_shutdown(handle: Box<TelemetryWorkerHandle>) {
    handle.wait_for_shutdown()
}

#[unsafe(no_mangle)]
/// This function takes ownership of the handle. It should not be used after calling it
pub extern "C" fn ddog_telemetry_handle_wait_for_shutdown_ms(
    handle: Box<TelemetryWorkerHandle>,
    wait_for_ms: u64,
) {
    handle.wait_for_shutdown_deadline(
        std::time::Instant::now() + std::time::Duration::from_millis(wait_for_ms),
    )
}

#[unsafe(no_mangle)]
/// Drops the handle without waiting for shutdown. The worker will continue running in the
/// background until it exits by itself
pub extern "C" fn ddog_telemetry_handle_drop(handle: Box<TelemetryWorkerHandle>) {
    drop(handle);
}
