// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[path = "../examples/support/mod.rs"]
mod support;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use libdd_telemetry::{
    data::{
        Application, Host,
        metrics::{MetricNamespace, MetricType},
    },
    signal_safe::{Error, MetricSeriesRef, MetricsRequest, send_metrics},
};
use reqwless::client::{HttpConnection, HttpResource};
use support::{FixedConnection, poll_once};

// The workspace disables assert_no_alloc's default features so this also
// enforces the check in release builds. Only this test binary uses the allocator.
#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn submits_metrics_without_allocator_calls() {
    let application = Application {
        service_name: "telemetry-test".to_owned(),
        language_name: "rust".to_owned(),
        tracer_version: "1.0.0".to_owned(),
        ..Application::default()
    };
    let host = Host {
        hostname: "localhost".to_owned(),
        ..Host::default()
    };
    let tags = ["component:signal-safe", "standalone"];
    let points = [(1, 1.0)];
    let series = [MetricSeriesRef {
        namespace: MetricNamespace::Telemetry,
        metric: "submission.count",
        points: &points,
        tags: &tags,
        common: false,
        metric_type: MetricType::Count,
        interval: 10,
    }];
    let request = MetricsRequest {
        tracer_time: 1,
        runtime_id: "runtime",
        seq_id: 1,
        application: &application,
        host: &host,
        origin: None,
        series: &series,
    };

    for (response, status) in [
        (
            &b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n"[..],
            202,
        ),
        (
            &b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n"[..],
            500,
        ),
    ] {
        let mut connection = FixedConnection::new(response);
        let mut resource = HttpResource {
            conn: HttpConnection::Plain(&mut connection),
            host: "localhost",
            base_path: "",
        };
        let mut body_buffer = [0_u8; 2_048];
        let mut response_buffer = [0_u8; 256];

        // Build identities and buffers above; guard the entire encoding and
        // submission, including all polling and both success and error paths.
        let result = assert_no_alloc(|| {
            poll_once(send_metrics(
                &mut resource,
                "/telemetry/proxy/api/v2/apmtelemetry",
                &request,
                &[("DD-API-KEY", "secret")],
                &mut body_buffer,
                &mut response_buffer,
            ))
        })
        .expect("the in-memory transport must complete immediately");

        if status == 202 {
            assert_eq!(result.unwrap(), status);
        } else {
            assert!(matches!(result, Err(Error::UnexpectedStatus(500))));
        }
        let written = std::str::from_utf8(connection.request()).unwrap();
        assert!(written.starts_with("POST /telemetry/proxy/api/v2/apmtelemetry HTTP/1.1\r\n"));
        assert!(written.contains("dd-client-library-language: rust\r\n"));
        assert!(written.contains("DD-API-KEY: secret\r\n"));
        let (_, body) = written.split_once("\r\n\r\n").unwrap();
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["payload"]["series"][0]["metric"], "submission.count");
        assert_eq!(
            body["payload"]["series"][0]["tags"][0],
            "component:signal-safe"
        );
        assert_eq!(body["payload"]["series"][0]["tags"][1], "standalone");
    }
}
