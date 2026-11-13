// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::catch_panic;
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
#[named]
pub unsafe extern "C" fn ddog_telemetry_handle_add_config(
    handle: &TelemetryWorkerHandle,
    name: ffi::CharSlice,
    value: ffi::Option<ffi::CharSlice>,
    origin: ConfigurationOrigin,
    config_id: ffi::CharSlice,
    seq_id: ffi::Option<u64>,
) -> MaybeError {
    catch_panic!({
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
    })
}

/// * version: an empty slice means no version
#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
#[named]
pub unsafe extern "C" fn ddog_telemetry_handle_add_product_change(
    handle: &TelemetryWorkerHandle,
    product: ffi::CharSlice,
    enabled: bool,
    version: ffi::CharSlice,
) -> MaybeError {
    catch_panic!({
        let product = crate::try_c!(product.try_to_string());
        let version = crate::try_c!(version.try_to_string_option());
        crate::try_c!(handle.add_product_change(product, enabled, version));
        MaybeError::None
    })
}

/// * path: an empty slice means no path
/// * request_body_type, response_body_type, response_code: an empty slice means not set
#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
#[named]
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
    catch_panic!({
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
    })
}

/// Flushes aggregated metrics into the pending payload queue
#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
#[named]
pub unsafe extern "C" fn ddog_telemetry_handle_flush_metrics(
    handle: &TelemetryWorkerHandle,
) -> MaybeError {
    catch_panic!({
        crate::try_c!(handle.try_send_msg(TelemetryActions::Lifecycle(
            LifecycleAction::FlushMetricAggr
        )));
        MaybeError::None
    })
}

/// Sends all pending telemetry data
#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
#[named]
pub unsafe extern "C" fn ddog_telemetry_handle_flush_data(
    handle: &TelemetryWorkerHandle,
) -> MaybeError {
    catch_panic!({
        crate::try_c!(handle.try_send_msg(TelemetryActions::Lifecycle(LifecycleAction::FlushData)));
        MaybeError::None
    })
}

/// Sends an app-extended-heartbeat message
#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
#[named]
pub unsafe extern "C" fn ddog_telemetry_handle_send_extended_heartbeat(
    handle: &TelemetryWorkerHandle,
) -> MaybeError {
    catch_panic!({
        crate::try_c!(handle.try_send_msg(TelemetryActions::Lifecycle(
            LifecycleAction::ExtendedHeartbeat
        )));
        MaybeError::None
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{find_sub_payloads, start_file_backed_worker};
    use ffi::tags::ddog_Vec_Tag_new;

    /// Sends config, product change, endpoint, flush and extended heartbeat messages through
    /// the FFI and checks each one reaches the worker output.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_handle_end_to_end() {
        unsafe {
            let (handle, f) = start_file_backed_worker();

            ddog_telemetry_handle_add_config(
                &handle,
                ffi::CharSlice::from("DD_TRACE_ENABLED"),
                ffi::Option::Some(ffi::CharSlice::from("true")),
                ConfigurationOrigin::EnvVar,
                ffi::CharSlice::from(""),
                ffi::Option::Some(3),
            )
            .unwrap_none();
            ddog_telemetry_handle_add_product_change(
                &handle,
                ffi::CharSlice::from("appsec"),
                true,
                ffi::CharSlice::from("1.0"),
            )
            .unwrap_none();
            let req = [ffi::CharSlice::from("application/json")];
            let codes = [200u32, 404];
            ddog_telemetry_handle_add_endpoint(
                &handle,
                ffi::Option::Some(Method::Get),
                ffi::CharSlice::from("/users"),
                ffi::CharSlice::from("http.request"),
                ffi::CharSlice::from("GET /users"),
                ffi::Slice::from(&req[..]),
                ffi::Slice::empty(),
                ffi::Slice::from(&codes[..]),
            )
            .unwrap_none();
            ddog_telemetry_handle_add_config(
                &handle,
                ffi::CharSlice::from("DD_EMPTY"),
                ffi::Option::Some(ffi::CharSlice::from("")),
                ConfigurationOrigin::Code,
                ffi::CharSlice::from(""),
                ffi::Option::None,
            )
            .unwrap_none();
            ddog_telemetry_handle_add_config(
                &handle,
                ffi::CharSlice::from("DD_NULL"),
                ffi::Option::None,
                ConfigurationOrigin::Code,
                ffi::CharSlice::from(""),
                ffi::Option::None,
            )
            .unwrap_none();

            let context_key = ddog_telemetry_handle_register_metric_context(
                &handle,
                ffi::CharSlice::from("flush_metric"),
                MetricType::Count,
                ddog_Vec_Tag_new(),
                true,
                MetricNamespace::Apm,
            );
            ddog_telemetry_handle_add_point(&handle, &context_key, 2.0).unwrap_none();
            ddog_telemetry_handle_flush_metrics(&handle).unwrap_none();
            ddog_telemetry_handle_flush_data(&handle).unwrap_none();

            // The flushes alone (before stop) must write the metric and pending payloads.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let flushed = loop {
                let out = std::fs::read_to_string(f.path()).unwrap();
                let has_metric = find_sub_payloads(&out, "generate-metrics").iter().any(|p| {
                    p["payload"]["series"]
                        .as_array()
                        .is_some_and(|s| s.iter().any(|m| m["metric"] == "flush_metric"))
                });
                let has_endpoint = !find_sub_payloads(&out, "app-endpoints").is_empty();
                if has_metric && has_endpoint {
                    break true;
                }
                if std::time::Instant::now() > deadline {
                    break false;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            };
            assert!(
                flushed,
                "flush did not write metrics/endpoints before shutdown"
            );

            ddog_telemetry_handle_send_extended_heartbeat(&handle).unwrap_none();

            ddog_telemetry_handle_stop(&handle).unwrap_none();
            ddog_telemetry_handle_wait_for_shutdown(handle);

            let output = std::fs::read_to_string(f.path()).unwrap();
            let conf = find_sub_payloads(&output, "app-client-configuration-change");
            let c = conf
                .iter()
                .filter_map(|p| p["payload"]["configuration"].as_array())
                .flatten()
                .find(|c| c["name"] == "DD_TRACE_ENABLED")
                .unwrap_or_else(|| panic!("config missing:\n{output}"));
            assert_eq!(c["value"], "true");
            assert_eq!(c["origin"], "env_var");
            assert_eq!(c["seq_id"], 3);
            let all_conf: Vec<&serde_json::Value> = conf
                .iter()
                .filter_map(|p| p["payload"]["configuration"].as_array())
                .flatten()
                .collect();
            let empty = all_conf.iter().find(|c| c["name"] == "DD_EMPTY").unwrap();
            assert_eq!(empty["value"], "");
            let null = all_conf.iter().find(|c| c["name"] == "DD_NULL").unwrap();
            assert!(null["value"].is_null());

            let prod = find_sub_payloads(&output, "app-product-change");
            assert!(
                prod.iter()
                    .any(|p| p["payload"]["products"]["appsec"]["enabled"] == true),
                "product change missing:\n{output}"
            );

            let eps = find_sub_payloads(&output, "app-endpoints");
            let e = eps
                .iter()
                .filter_map(|p| p["payload"]["endpoints"].as_array())
                .flatten()
                .find(|e| e["resource_name"] == "GET /users")
                .unwrap_or_else(|| panic!("endpoint missing:\n{output}"));
            assert_eq!(e["method"], "GET");
            assert_eq!(e["path"], "/users");
            assert_eq!(e["response_code"], serde_json::json!([200, 404]));
            assert_eq!(
                e["request_body_type"],
                serde_json::json!(["application/json"])
            );

            assert!(
                !find_sub_payloads(&output, "app-extended-heartbeat").is_empty(),
                "extended heartbeat missing:\n{output}"
            );
        }
    }
}
