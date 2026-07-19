// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{process::ExitCode, str, time::UNIX_EPOCH};

use libdd_telemetry::{
    data::metrics::{MetricNamespace, MetricType},
    signal_safe::{
        encode_metrics, ApplicationRef, HostRef, MetricSeriesRef, MetricsRequest, TagRef,
    },
};

fn main() -> ExitCode {
    let timestamp = UNIX_EPOCH
        .elapsed()
        .map_or(0, |duration| duration.as_secs());
    let tags = match TagRef::new("component", "signal-safe") {
        Ok(tag) => [tag],
        Err(error) => {
            eprintln!("invalid tag: {error}");
            return ExitCode::FAILURE;
        }
    };
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
        application: ApplicationRef {
            service_name: "libdd-telemetry-example",
            language_name: "rust",
            language_version: "unknown",
            tracer_version: env!("CARGO_PKG_VERSION"),
            ..ApplicationRef::default()
        },
        host: HostRef {
            hostname: "unknown_hostname",
            ..HostRef::default()
        },
        origin: None,
        series: &series,
    };

    let mut body_buffer = [0_u8; 2_048];
    let body_len = match encode_metrics(&request, &mut body_buffer) {
        Ok(length) => length,
        Err(error) => {
            eprintln!("telemetry serialization failed: {error:?}");
            return ExitCode::FAILURE;
        }
    };
    match str::from_utf8(&body_buffer[..body_len]) {
        Ok(body) => {
            println!("{body}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("telemetry serialization produced invalid UTF-8: {error}");
            ExitCode::FAILURE
        }
    }
}
