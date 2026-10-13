// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(feature = "external-observations")]

use httpmock::MockServer;
use libdd_capabilities_impl::NativeCapabilities;
use libdd_data_pipeline::trace_exporter::{TraceExporter, TraceExporterOutputFormat};
use libdd_shared_runtime::ForkSafeRuntime;
use libdd_trace_utils::span::{span_pool::PooledChunks, v04::SpanBytes};
use tokio_util::sync::CancellationToken;

#[cfg_attr(miri, ignore = "httpmock retains detached server threads")]
#[test]
fn reports_terminal_results_without_native_delivery() {
    for format in [
        TraceExporterOutputFormat::V04,
        TraceExporterOutputFormat::V05,
    ] {
        for status in [200, 503] {
            let server = MockServer::start();
            let path = match format {
                TraceExporterOutputFormat::V05 => "/v0.5/traces",
                _ => "/v0.4/traces",
            };
            let request = server.mock(|when, then| {
                when.method("POST").path(path);
                then.status(status).body("{}");
            });
            let telemetry = server.mock(|when, then| {
                when.path("/telemetry/proxy/api/v2/apmtelemetry");
                then.status(200);
            });
            let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
            builder
                .set_url(&server.base_url())
                .set_output_format(format)
                .enable_observations();
            #[cfg(feature = "telemetry")]
            builder.enable_telemetry(Default::default());
            let exporter = builder.build::<NativeCapabilities>().unwrap();
            for _ in 0..2 {
                let chunks = vec![
                    vec![SpanBytes::default(), SpanBytes::default()],
                    vec![SpanBytes::default()],
                ];
                let (result, report) =
                    exporter.send_trace_chunks_observed(PooledChunks::unpooled(chunks), None);
                let payload = &report;
                assert_eq!(result.is_ok(), status == 200);
                assert_eq!((payload.status_code, payload.responses_count), (status, 1));
                assert_eq!(payload.spans_enqueued_for_serialization, 3);
                assert_eq!(payload.errors_status_code, u64::from(status == 503));
                assert_eq!(payload.chunks_sent, if status == 200 { 2 } else { 0 });
                assert_eq!(
                    payload.chunks_dropped_send_failure,
                    if status == 503 { 2 } else { 0 }
                );
                assert_eq!(
                    payload.spans_dropped_api_error,
                    if status == 503 { 3 } else { 0 }
                );
                assert_eq!(payload.bytes_sent > 0, status == 200);
                assert!(payload.requests_count >= 1);
                if status == 503 {
                    assert!(payload.requests_count > 1);
                }
                assert_eq!(report.chunks_dropped_p0, 0);
                assert_eq!(report.chunks_dropped_by_trace_filter, 0);
            }
            assert!(request.calls() >= 2);
            let (result, stats) = exporter.shutdown_observed(None);
            result.unwrap();
            assert_eq!(stats, [0; 16]);
            telemetry.assert_calls(0);
        }
    }
}

#[cfg_attr(miri, ignore = "httpmock retains detached server threads")]
#[test]
fn cancellation_has_no_invented_terminal_result() {
    let server = MockServer::start();
    let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
    builder.set_url(&server.base_url()).enable_observations();
    let exporter = builder.build::<NativeCapabilities>().unwrap();
    let token = CancellationToken::new();
    token.cancel();
    let (result, report) = exporter.send_trace_chunks_observed(
        PooledChunks::unpooled(vec![vec![SpanBytes::default()]]),
        Some(&token),
    );
    assert!(result.is_err());
    assert_eq!(report.requests_count, 0);
    assert_eq!(report.bytes_sent, 0);
    assert_eq!(report.spans_enqueued_for_serialization, 0);
    exporter.shutdown(None).unwrap();
}

#[cfg_attr(miri, ignore = "httpmock retains detached server threads")]
#[test]
fn failed_build_and_timeout_reports_do_not_depend_on_response_bodies() {
    for build_error in [true, false] {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.path("/v0.4/traces");
            then.status(200)
                .body("{}")
                .delay(std::time::Duration::from_millis(50));
        });
        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
        builder
            .set_url(&server.base_url())
            .set_connection_timeout(Some(1))
            .enable_observations();
        if build_error {
            builder.set_test_session_token("invalid\nheader");
        }
        let exporter = builder.build::<NativeCapabilities>().unwrap();
        let (result, report) = exporter.send_trace_chunks_observed(
            PooledChunks::unpooled(vec![vec![SpanBytes::default()]]),
            None,
        );
        assert!(result.is_err());
        assert_eq!(report.spans_enqueued_for_serialization, 1);
        assert_eq!((report.status_code, report.responses_count), (0, 0));
        assert_eq!(report.bytes_sent, 0);
        assert_eq!(report.errors_network, 0);
        if build_error {
            assert_eq!(report.requests_count, 1);
            assert_eq!(report.chunks_dropped_serialization_error, 1);
            assert_eq!(report.spans_dropped_serialization_error, 1);
            assert_eq!(report.errors_timeout, 0);
        } else {
            assert!(report.requests_count > 1);
            assert_eq!(report.errors_timeout, 1);
            assert_eq!(report.chunks_dropped_send_failure, 1);
            assert_eq!(report.spans_dropped_api_error, 1);
        }
        exporter.shutdown(None).unwrap();
    }
}

#[cfg_attr(
    miri,
    ignore = "wall-clock network deadlines are unreliable under Miri"
)]
#[test]
fn retry_success_counts_attempts_but_only_the_final_response() {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut traces = 0;
        while traces < 2 {
            let (mut client, _) = listener.accept().unwrap();
            client
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&mut client);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let trace_request = line.contains("/v0.4/traces");
            let mut length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            reader.read_exact(&mut vec![0; length]).unwrap();
            if trace_request {
                traces += 1;
            }
            let status = if trace_request && traces == 1 {
                503
            } else {
                200
            };
            write!(
                client,
                "HTTP/1.1 {status} Test\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            )
            .unwrap();
        }
        traces
    });
    let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
    builder.set_url(&url).enable_observations();
    let exporter = builder.build::<NativeCapabilities>().unwrap();
    let (result, report) = exporter
        .send_trace_chunks_observed(PooledChunks::unpooled(Vec::<Vec<SpanBytes>>::new()), None);
    result.unwrap();
    assert_eq!(report.requests_count, 2);
    assert_eq!(report.errors_status_code, 0);
    assert_eq!((report.status_code, report.responses_count), (200, 1));
    assert_eq!(report.bytes_sent, 1);
    exporter.shutdown(None).unwrap();
    assert_eq!(server.join().unwrap(), 2);
}

#[cfg(unix)]
#[cfg_attr(miri, ignore = "Miri does not support AF_UNIX sockets")]
#[test]
fn unavailable_socket_produces_a_network_report() {
    let dir = tempfile::tempdir().unwrap();
    let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
    builder
        .set_url(&format!("unix://{}/missing.sock", dir.path().display()))
        .enable_observations();
    let exporter = builder.build::<NativeCapabilities>().unwrap();
    let (result, report) = exporter.send_trace_chunks_observed(
        PooledChunks::unpooled(vec![vec![SpanBytes::default()]]),
        None,
    );
    assert!(result.is_err());
    assert!(report.requests_count > 1);
    assert_eq!(report.errors_network, 1);
    assert_eq!(report.errors_timeout, 0);
    assert_eq!(report.chunks_dropped_send_failure, 1);
    assert_eq!(report.spans_dropped_api_error, 1);
    assert_eq!((report.status_code, report.responses_count), (0, 0));
    exporter.shutdown(None).unwrap();
}

#[cfg_attr(miri, ignore = "httpmock retains detached server threads")]
#[test]
fn empty_input_reports_an_empty_payload() {
    let server = MockServer::start();
    let request = server.mock(|when, then| {
        when.path("/v0.4/traces");
        then.status(200).body("{}");
    });
    let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
    builder.set_url(&server.base_url()).enable_observations();
    let exporter = builder.build::<NativeCapabilities>().unwrap();
    let (result, report) = exporter
        .send_trace_chunks_observed(PooledChunks::unpooled(Vec::<Vec<SpanBytes>>::new()), None);
    result.unwrap();
    assert_eq!(report.requests_count, 1);
    assert_eq!(report.bytes_sent, 1);
    assert_eq!(report.chunks_sent, 0);
    assert_eq!(report.spans_enqueued_for_serialization, 0);
    request.assert_calls(1);
    exporter.shutdown(None).unwrap();
}

#[cfg(feature = "test-utils")]
#[cfg_attr(miri, ignore = "httpmock retains detached server threads")]
#[tokio::test]
async fn css_filtering_reports_counts_before_the_empty_payload_send() {
    use std::time::Duration;
    let server = MockServer::start_async().await;
    server.mock_async(|when, then| {
        when.path("/info");
        then.status(200).body(r#"{"endpoints":["/v0.6/stats"],"client_drop_p0s":true,"filter_tags":{"reject":["drop:true"]}}"#);
    }).await;
    server
        .mock_async(|when, then| {
            when.path("/v0.4/traces");
            then.status(200).body("{}");
        })
        .await;
    server
        .mock_async(|when, then| {
            when.path("/v0.6/stats");
            then.status(200);
        })
        .await;
    let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
    builder
        .set_url(&server.base_url())
        .enable_stats(Duration::from_secs(60))
        .enable_observations();
    let exporter = builder.build_async::<NativeCapabilities>().await.unwrap();
    exporter
        .wait_agent_info_ready(Duration::from_secs(5))
        .await
        .unwrap();
    let mut rejected = SpanBytes::default();
    rejected.meta.insert("drop".into(), "true".into());
    let mut p0 = SpanBytes::default();
    p0.metrics.insert("_sampling_priority_v1".into(), 0.0);
    let exporter = std::sync::Arc::new(exporter);
    let sender = exporter.clone();
    let (result, report) = tokio::task::spawn_blocking(move || {
        sender.send_trace_chunks_observed(
            PooledChunks::unpooled(vec![vec![rejected], vec![p0]]),
            None,
        )
    })
    .await
    .unwrap();
    result.unwrap();
    assert_eq!(report.chunks_dropped_by_trace_filter, 1);
    assert_eq!(report.chunks_dropped_p0, 1);
    let payload = &report;
    assert_eq!(payload.chunks_sent, 0);
    assert_eq!(payload.spans_enqueued_for_serialization, 0);
    assert_eq!(payload.bytes_sent, 1);
    assert!(exporter.flush_client_side_stats_async().await);
    assert_eq!(exporter.take_stats_observations(), [0; 16]);
    let exporter = std::sync::Arc::into_inner(exporter).unwrap();
    tokio::task::spawn_blocking(move || exporter.shutdown(None))
        .await
        .unwrap()
        .unwrap();
}

#[cfg(feature = "telemetry")]
#[cfg_attr(miri, ignore = "httpmock retains detached server threads")]
#[test]
fn native_delivery_and_external_reports_describe_the_same_send() {
    use std::sync::{Arc, Mutex};
    let server = MockServer::start();
    let bodies = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let received = bodies.clone();
    server.mock(|when, then| {
        when.method("POST").path("/v0.4/traces");
        then.status(200).body("{}");
    });
    server.mock(|when, then| {
        when.path("/telemetry/proxy/api/v2/apmtelemetry")
            .is_true(move |request| {
                if request.uri().path() == "/telemetry/proxy/api/v2/apmtelemetry" {
                    if let Ok(body) = serde_json::from_slice(request.body().as_ref()) {
                        received.lock().unwrap().push(body);
                    }
                }
                true
            });
        then.status(200);
    });
    let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
    builder
        .set_url(&server.base_url())
        .set_service("parity")
        .set_language("ruby")
        .set_language_version("4.0")
        .set_tracer_version("test")
        .enable_telemetry(Default::default());
    let native = builder.build::<NativeCapabilities>().unwrap();
    let chunks = vec![vec![SpanBytes::default(), SpanBytes::default()]];
    native
        .send_trace_chunks(PooledChunks::unpooled(chunks.clone()), None)
        .unwrap();
    native.shutdown(None).unwrap();
    let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
    builder
        .set_url(&server.base_url())
        .set_service("parity")
        .set_language("ruby")
        .set_language_version("4.0")
        .set_tracer_version("test")
        .enable_observations();
    let external = builder.build::<NativeCapabilities>().unwrap();
    let (result, report) =
        external.send_trace_chunks_observed(PooledChunks::unpooled(chunks), None);
    result.unwrap();
    external.shutdown(None).unwrap();
    let bodies = bodies.lock().unwrap();
    let text = serde_json::to_string(&*bodies).unwrap();
    for (name, value) in [
        ("trace_api.requests", report.requests_count),
        ("trace_api.responses", report.responses_count),
        ("trace_chunks_sent", report.chunks_sent),
        (
            "spans_enqueued_for_serialization",
            report.spans_enqueued_for_serialization,
        ),
    ] {
        fn find_series<'a>(
            value: &'a serde_json::Value,
            name: &str,
        ) -> Option<&'a serde_json::Value> {
            if value.get("metric").and_then(|v| v.as_str()) == Some(name) {
                return Some(value);
            }
            match value {
                serde_json::Value::Array(values) => {
                    values.iter().find_map(|v| find_series(v, name))
                }
                serde_json::Value::Object(values) => {
                    values.values().find_map(|v| find_series(v, name))
                }
                _ => None,
            }
        }
        let metric = bodies
            .iter()
            .find_map(|body| find_series(body, name))
            .unwrap_or_else(|| panic!("missing {name}: {text}"));
        assert_eq!(metric["points"][0][1].as_f64(), Some(value as f64));
        assert_eq!(metric["type"], "count");
        assert_eq!(metric["common"], true);
        if name == "spans_enqueued_for_serialization" {
            assert_eq!(metric["tags"], serde_json::json!([]));
        } else {
            assert!(
                metric["tags"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!("src_library:libdatadog"))
            );
        }
    }
    assert!(text.contains("trace_api.bytes"));
    assert!(text.contains("sketch_b64"));
    assert!(report.bytes_sent > 0);
}
