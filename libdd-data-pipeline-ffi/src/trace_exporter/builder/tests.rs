// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::error::ddog_trace_exporter_error_free;
use crate::response::ExporterResponse;
use crate::trace_exporter::{ddog_trace_exporter_free, ddog_trace_exporter_send};
use httpmock::MockServer;
use httpmock::prelude::*;
use libdd_common_ffi::slice::ByteSlice;
use libdd_shared_runtime::SharedRuntime;
use std::mem::MaybeUninit;

type StringSetter = unsafe extern "C" fn(
    Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    CharSlice<'_>,
) -> Option<Box<ExporterError>>;

// Excluded from miri_tests: prohibitively slow under Miri (75s: 20 setters x 3
// invocations each, every invocation Debug-formatting the whole builder).
#[test]
fn config_string_setters() {
    let setters: &[(StringSetter, &str)] = &[
        (ddog_trace_exporter_builder_set_url, "http://localhost:8126"),
        (ddog_trace_exporter_builder_set_tracer_version, "1.0"),
        (ddog_trace_exporter_builder_set_language, "lang"),
        (ddog_trace_exporter_builder_set_language_version, "0.1"),
        (
            ddog_trace_exporter_builder_set_language_interpreter,
            "interpreter",
        ),
        (ddog_trace_exporter_builder_set_hostname, "hostname"),
        (ddog_trace_exporter_builder_set_env, "test"),
        (ddog_trace_exporter_builder_set_app_version, "1.2"),
        (ddog_trace_exporter_builder_set_service, "service"),
        (ddog_trace_exporter_builder_set_process_tags, "key:val"),
        (ddog_trace_exporter_builder_set_runtime_id, "runtime-id"),
        (ddog_trace_exporter_builder_set_test_session_token, "token"),
        (
            ddog_trace_exporter_builder_set_otlp_endpoint,
            "http://localhost:4318/v1/traces",
        ),
        (
            ddog_trace_exporter_builder_set_otlp_protocol,
            "http/protobuf",
        ),
        (
            ddog_trace_exporter_builder_set_span_obfuscation_config,
            r#"{"http":{"remove_query_string":true}}"#,
        ),
        (
            ddog_trace_exporter_builder_set_dogstatsd_url,
            "http://localhost:8125",
        ),
        (ddog_trace_exporter_builder_set_git_commit_sha, "797e9ea"),
        (ddog_trace_exporter_builder_set_container_id, "cid"),
        (
            ddog_trace_exporter_builder_set_language_interpreter_vendor,
            "vendor",
        ),
        (
            ddog_trace_exporter_builder_set_otlp_metrics_endpoint,
            "http://localhost:4318/v1/metrics",
        ),
    ];
    for &(setter, value) in setters {
        unsafe {
            let error = setter(None, CharSlice::from(value));
            assert_eq!(error.unwrap().code, ErrorCode::InvalidArgument);

            let mut config = TraceExporterBuilder::default();
            let before = format!("{config:?}");
            assert_eq!(setter(Some(&mut config), CharSlice::from(value)), None);
            let configured = format!("{config:?}");
            assert_ne!(
                configured, before,
                "setter must update the builder: {value}"
            );

            let error = setter(Some(&mut config), CharSlice::from_bytes(&[0xff]));
            assert_eq!(error.unwrap().code, ErrorCode::InvalidInput);
            assert_eq!(
                format!("{config:?}"),
                configured,
                "invalid input must leave the builder unchanged"
            );
        }
    }
}

#[test]
fn config_shared_runtime_ownership() {
    unsafe {
        let runtime = Arc::new(ForkSafeRuntime::new().unwrap());
        let raw = Arc::into_raw(runtime.clone());
        let handle = NonNull::new(raw.cast_mut());
        assert_eq!(
            ddog_trace_exporter_builder_set_shared_runtime(None, handle)
                .unwrap()
                .code,
            ErrorCode::InvalidArgument
        );
        let mut config = Box::<TraceExporterBuilder<ForkSafeRuntime>>::default();
        assert_eq!(
            ddog_trace_exporter_builder_set_shared_runtime(Some(&mut config), None)
                .unwrap()
                .code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            ddog_trace_exporter_builder_set_shared_runtime(Some(&mut config), handle),
            None
        );
        assert_eq!(Arc::strong_count(&runtime), 3);
        drop(Arc::from_raw(raw));
        ddog_trace_exporter_builder_free(config);
        assert_eq!(Arc::strong_count(&runtime), 1);
    }
}

#[cfg_attr(miri, ignore)]
#[test]
fn exporter_constructor_test() {
    unsafe {
        let mut config = Box::<TraceExporterBuilder<ForkSafeRuntime>>::default();
        assert_eq!(
            ddog_trace_exporter_builder_set_output_to_log(Some(&mut config), 0),
            None
        );
        let mut exporter = MaybeUninit::<Box<TraceExporter>>::uninit();
        assert_eq!(
            ddog_trace_exporter_builder_build(Some(config), NonNull::from(&mut exporter).cast()),
            None
        );
        ddog_trace_exporter_free(exporter.assume_init());
    }
}

#[cfg_attr(miri, ignore)]
#[test]
fn exporter_constructor_error_consumes_config() {
    unsafe {
        let runtime = Arc::new(ForkSafeRuntime::new().unwrap());
        let mut config = Box::<TraceExporterBuilder<ForkSafeRuntime>>::default();
        config.set_shared_runtime(runtime.clone());
        assert_eq!(
            ddog_trace_exporter_builder_set_agentless_timeout(Some(&mut config), 1000),
            None
        );
        let mut exporter = MaybeUninit::<Box<TraceExporter>>::uninit();
        let error =
            ddog_trace_exporter_builder_build(Some(config), NonNull::from(&mut exporter).cast());
        assert!(
            error.is_some(),
            "a timeout without an agentless endpoint must fail"
        );
        ddog_trace_exporter_error_free(error);
        assert_eq!(
            Arc::strong_count(&runtime),
            1,
            "failed builds must drop the builder"
        );
    }
}

fn agent_config(url: &str) -> Box<TraceExporterBuilder<ForkSafeRuntime>> {
    let mut config = Box::<TraceExporterBuilder<ForkSafeRuntime>>::default();
    let setters: &[(StringSetter, &str)] = &[
        (ddog_trace_exporter_builder_set_url, url),
        (ddog_trace_exporter_builder_set_tracer_version, "0.1"),
        (ddog_trace_exporter_builder_set_language, "lang"),
        (ddog_trace_exporter_builder_set_language_version, "0.1"),
        (
            ddog_trace_exporter_builder_set_language_interpreter,
            "interpreter",
        ),
        (ddog_trace_exporter_builder_set_hostname, "hostname"),
        (ddog_trace_exporter_builder_set_env, "env-test"),
        (ddog_trace_exporter_builder_set_app_version, "1.0"),
        (ddog_trace_exporter_builder_set_service, "test-service"),
    ];
    for &(setter, value) in setters {
        assert_eq!(
            unsafe { setter(Some(&mut config), CharSlice::from(value)) },
            None
        );
    }
    config
}

#[test]
#[cfg_attr(miri, ignore)]
fn exporter_send_empty_array_test() {
    // Test added due to ensure the exporter is able to send empty arrays because some tracers
    // (.NET) ping the agent with the aforementioned data type.
    unsafe {
        let server = MockServer::start();
        let response_body = r#"{
                    "rate_by_service": {
                        "service:foo,env:staging": 1.0,
                        "service:,env:": 0.8
                    }
                }"#;

        let mock_traces = server.mock(|when, then| {
            when.method(POST)
                .header("Content-type", "application/msgpack")
                .header("Datadog-Meta-Tracer-Version", "0.1")
                .header("Datadog-Meta-Lang", "lang")
                .header("Datadog-Meta-Lang-Version", "0.1")
                .header("Datadog-Meta-Lang-Interpreter", "interpreter")
                .path("/v0.4/traces");
            then.status(200).body(response_body);
        });

        let cfg = agent_config(&server.url("/"));

        let mut ptr: MaybeUninit<Box<TraceExporter>> = MaybeUninit::uninit();
        let mut ret =
            ddog_trace_exporter_builder_build(Some(cfg), NonNull::new_unchecked(&mut ptr).cast());

        assert_eq!(ret, None);
        let exporter = ptr.assume_init();

        let data = vec![0x90];
        let traces = ByteSlice::new(&data);
        let mut response: MaybeUninit<Box<ExporterResponse>> = MaybeUninit::uninit();

        ret = ddog_trace_exporter_send(
            Some(exporter.as_ref()),
            traces,
            Some(NonNull::new_unchecked(&mut response).cast()),
        );
        mock_traces.assert();
        assert_eq!(ret, None);
        assert_eq!(
            String::from_utf8_lossy(&response.assume_init().body.unwrap()),
            response_body
        );

        ddog_trace_exporter_free(exporter);
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn exporter_send_telemetry_test() {
    unsafe {
        let server = MockServer::start();
        let response_body = r#"{
                    "rate_by_service": {
                        "service:foo,env:staging": 1.0,
                        "service:,env:": 0.8
                    }
                }"#;
        let mock_traces = server.mock(|when, then| {
            when.method(POST).path("/v0.4/traces");
            then.status(200)
                .header("content-type", "application/json")
                .body(response_body);
        });

        let mock_metrics = server.mock(|when, then| {
            when.method(POST)
                .path("/telemetry/proxy/api/v2/apmtelemetry")
                .header("dd-session-id", "session")
                .header("dd-root-session-id", "root")
                .header("dd-parent-session-id", "parent")
                .body_includes(r#""runtime_id":"foo""#)
                .body_includes(r#""metric":"trace_api."#);
            then.status(200)
                .header("content-type", "application/json")
                .body("");
        });

        let mut cfg = agent_config(&server.url("/"));
        assert_eq!(
            ddog_trace_exporter_builder_enable_telemetry(
                Some(&mut cfg),
                Some(&ExporterTelemetryConfig {
                    heartbeat: 10000,
                    debug_enabled: true,
                }),
            ),
            None
        );
        assert_eq!(
            ddog_trace_exporter_builder_set_telemetry_instrumentation_sessions(
                Some(&mut cfg),
                CharSlice::from("session"),
                CharSlice::from("root"),
                CharSlice::from("parent"),
            ),
            None
        );

        let error =
            ddog_trace_exporter_builder_set_runtime_id(Some(&mut cfg), CharSlice::from("foo"));
        assert!(error.is_none());

        let mut ptr: MaybeUninit<Box<TraceExporter>> = MaybeUninit::uninit();
        let mut ret =
            ddog_trace_exporter_builder_build(Some(cfg), NonNull::new_unchecked(&mut ptr).cast());

        assert_eq!(ret, None);
        let exporter = ptr.assume_init();

        let data = vec![0x90];
        let traces = ByteSlice::new(&data);
        let mut response: MaybeUninit<Box<ExporterResponse>> = MaybeUninit::uninit();

        ret = ddog_trace_exporter_send(
            Some(exporter.as_ref()),
            traces,
            Some(NonNull::new_unchecked(&mut response).cast()),
        );
        mock_traces.assert();
        assert_eq!(ret, None);
        assert_eq!(
            String::from_utf8_lossy(&response.assume_init().body.unwrap()),
            response_body
        );

        ddog_trace_exporter_free(exporter);
        // It should receive 1 metrics payload (excluding heartbeats)
        mock_metrics.assert_calls(1);
    }
}
