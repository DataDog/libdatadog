// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod support;

use std::time::UNIX_EPOCH;

use libdd_telemetry::{
    data::{
        Application, Host,
        metrics::{MetricNamespace, MetricType},
    },
    signal_safe::{MetricSeriesRef, MetricsRequest, TagRef, send_metrics},
};
use reqwless::client::{HttpConnection, HttpResource};
use support::{FixedConnection, poll_once};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let timestamp = UNIX_EPOCH
        .elapsed()
        .map_or(0, |duration| duration.as_secs());
    let application = Application {
        service_name: "libdd-telemetry-example".to_owned(),
        language_name: "rust".to_owned(),
        language_version: "unknown".to_owned(),
        tracer_version: env!("CARGO_PKG_VERSION").to_owned(),
        ..Application::default()
    };
    let host = Host {
        hostname: "unknown_hostname".to_owned(),
        ..Host::default()
    };
    let tags = [TagRef::new("component", "signal-safe")?];
    let points = [(timestamp, 1.0)];
    let series = [MetricSeriesRef {
        namespace: MetricNamespace::Telemetry,
        metric: "metrics_submissions",
        points: &points,
        tags: &tags,
        common: false,
        metric_type: MetricType::Count,
        interval: 10,
    }];
    let request = MetricsRequest {
        tracer_time: timestamp,
        runtime_id: "00000000-0000-0000-0000-000000000000",
        seq_id: 0,
        application: &application,
        host: &host,
        origin: None,
        series: &series,
    };
    // This demo uses an in-memory transport. A real caller supplies its own
    // connection, with application and host identity prepared beforehand.
    let mut connection =
        FixedConnection::new(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n");
    let mut resource = HttpResource {
        conn: HttpConnection::Plain(&mut connection),
        host: "localhost",
        base_path: "",
    };
    let mut body_buffer = [0_u8; 2_048];
    let mut response_buffer = [0_u8; 256];
    let status = poll_once(send_metrics(
        &mut resource,
        "/telemetry/proxy/api/v2/apmtelemetry",
        &request,
        &[],
        &mut body_buffer,
        &mut response_buffer,
    ))
    .ok_or("the in-memory transport unexpectedly returned Pending")??;

    println!("telemetry metric submitted, status={status}");
    println!("{}", std::str::from_utf8(connection.request())?);
    Ok(())
}
