// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(all(feature = "telemetry", not(target_arch = "wasm32"), not(miri)))]

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::time::Duration;

use httpmock::{Method::POST, MockServer};
use libdd_capabilities_impl::NativeCapabilities;
use libdd_data_pipeline::OtlpProtocol;
use libdd_data_pipeline::trace_exporter::{
    TelemetryConfig, TraceExporter, error::TraceExporterError,
};
use libdd_shared_runtime::ForkSafeRuntime;
use libdd_trace_utils::span::span_pool::PooledChunks;
use libdd_trace_utils::test_utils::create_test_no_alloc_span;
use serde_json::Value;

fn series(value: &Value, output: &mut Vec<Value>) {
    match value {
        Value::Object(fields) if fields.contains_key("metric") => output.push(value.clone()),
        Value::Object(fields) => fields.values().for_each(|value| series(value, output)),
        Value::Array(values) => values.iter().for_each(|value| series(value, output)),
        _ => {}
    }
}

fn count(metrics: &[Value], name: &str, tag: Option<&str>) -> f64 {
    metrics
        .iter()
        .filter(|metric| {
            metric["metric"] == name
                && tag.is_none_or(|tag| {
                    metric["tags"]
                        .as_array()
                        .is_some_and(|tags| tags.iter().any(|value| value == tag))
                })
        })
        .flat_map(|metric| metric["points"].as_array().expect("count points"))
        .map(|point| point[1].as_f64().expect("numeric count"))
        .sum()
}

#[test]
fn agent_response_accounting_matches_export_result() {
    check_response_accounting(None);
}

#[test]
fn otlp_json_response_accounting_matches_export_result() {
    check_response_accounting(Some(OtlpProtocol::HttpJson));
}

#[test]
fn otlp_protobuf_response_accounting_matches_export_result() {
    check_response_accounting(Some(OtlpProtocol::HttpProtobuf));
}

fn check_response_accounting(protocol: Option<OtlpProtocol>) {
    let error_attempts = if protocol.is_some() { 5 } else { 6 };
    let path = if protocol.is_some() {
        "/v1/traces"
    } else {
        "/v0.4/traces"
    };
    for (status, attempts) in [
        (200, 1u32),
        (202, 1),
        (204, 1),
        (302, 1),
        (307, 1),
        (400, error_attempts),
        (503, error_attempts),
    ] {
        let agent = MockServer::start();
        let redirect = agent.mock(|when, then| {
            when.path("/redirect-target");
            then.status(200);
        });
        let traces = agent.mock(|when, then| {
            when.method(POST).path(path);
            then.status(status)
                .header("Location", agent.url("/redirect-target"))
                .body(if status == 204 { "" } else { "response body" });
        });
        let (tx, rx) = mpsc::channel();
        let _telemetry = agent.mock(|when, then| {
            when.method(POST)
                .path("/telemetry/proxy/api/v2/apmtelemetry")
                .is_true(move |request| {
                    tx.send(
                        serde_json::from_slice::<Value>(&request.body_vec())
                            .expect("telemetry JSON"),
                    )
                    .is_ok()
                });
            then.status(200);
        });

        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
        if let Some(protocol) = protocol {
            builder
                .set_otlp_endpoint(&agent.url(path))
                .set_otlp_protocol(protocol);
        }
        builder
            .set_url(&agent.url("/"))
            .set_service("response-accounting")
            .set_language("test")
            .set_language_version("1.0")
            .set_tracer_version("1.0")
            .enable_telemetry(TelemetryConfig {
                heartbeat: 60_000,
                ..Default::default()
            });
        let exporter = builder.build::<NativeCapabilities>().unwrap();
        let data = PooledChunks::unpooled(vec![
            vec![
                create_test_no_alloc_span(1, 1, 0, 1, true),
                create_test_no_alloc_span(1, 2, 1, 1, false),
            ],
            vec![create_test_no_alloc_span(2, 3, 0, 1, true)],
        ]);
        let result = exporter.send_trace_chunks(data, None);
        if status < 300 {
            assert!(result.is_ok(), "status {status}: {result:?}");
        } else {
            match result {
                Err(TraceExporterError::Request(error)) => {
                    assert_eq!(error.status().as_u16(), status);
                    assert!(error.to_string().contains("response body"));
                }
                other => panic!("expected HTTP {status} error, got {other:?}"),
            }
        }

        exporter.shutdown(Some(Duration::from_secs(5))).unwrap();
        traces.assert_calls(usize::try_from(attempts).unwrap());
        redirect.assert_calls(0);
        // httpmock can evaluate a request matcher more than once.
        let mut batches = BTreeMap::new();
        for request in rx.try_iter() {
            batches.insert(
                request["seq_id"].as_u64().expect("telemetry sequence"),
                request,
            );
        }
        let mut metrics = Vec::new();
        for request in batches.values() {
            series(request, &mut metrics);
        }
        assert!(!metrics.is_empty(), "missing telemetry for status {status}");
        assert_eq!(
            count(&metrics, "trace_api.requests", None),
            f64::from(attempts)
        );
        assert_eq!(
            count(
                &metrics,
                "trace_api.responses",
                Some(&format!("status_code:{status}"))
            ),
            1.0
        );
        assert_eq!(
            count(&metrics, "spans_enqueued_for_serialization", None),
            3.0
        );
        let success = status < 300;
        assert_eq!(
            count(&metrics, "trace_chunks_sent", None),
            if success { 2.0 } else { 0.0 },
            "status {status}: {metrics:#?}"
        );
        assert_eq!(
            count(&metrics, "trace_api.errors", Some("type:status_code")),
            if success { 0.0 } else { 1.0 }
        );
        assert_eq!(
            count(
                &metrics,
                "trace_chunks_dropped",
                Some("reason:send_failure")
            ),
            if success { 0.0 } else { 2.0 }
        );
        assert_eq!(
            count(&metrics, "spans_dropped", Some("reason:api_error")),
            if success { 0.0 } else { 3.0 }
        );
        assert_eq!(
            metrics
                .iter()
                .any(|metric| metric["metric"] == "trace_api.bytes"),
            success
        );
    }
}
