// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{time::Duration, vec};

use data_pipeline::stats_exporter::{blocking, Configuration, LibraryMetadata, SpanStats};
use ddcommon::Endpoint;

fn main() {
    let stats_exporter = blocking::StatsExporter::new(
        LibraryMetadata {
            hostname: "libdatadog-test".into(),
            env: "test".into(),
            version: "0.0.0".into(),
            lang: "rust".into(),
            tracer_version: "0.0.0".into(),
            runtime_id: "e39d6d12-0752-489f-b488-cf80006c0378".into(),
            service: "stats_exporter_test".into(),
            container_id: "".into(),
            git_commit_sha: "".into(),
            tags: vec![],
        },
        Configuration {
            stats_computation_interval: Duration::from_secs(10),
            request_timeout: None,
            endpoint: Endpoint {
                url: hyper::Uri::from_static("http://localhost:8136/v0.6/stats"),
                api_key: None,
            },
        },
    )
    .unwrap();

    let span_stats = SpanStats {
        resource_name: "successful_op".into(),
        service_name: "stats_exporter_test".into(),
        operation_name: "insert_stats".into(),
        span_type: "".into(),
        http_status_code: 200,
        is_synthetics_request: false,
        is_error: false,
        is_top_level: true,
        duration: 1000000000,
    };

    for i in 0..100 {
        let mut s = span_stats.clone();
        s.duration += 10000000 * i;
        stats_exporter.insert(s)
    }

    for i in 0..100 {
        let mut s = span_stats.clone();
        s.resource_name = "error_op".into();
        s.is_error = true;
        s.http_status_code = 400;
        s.duration += 10000000 * i;
        stats_exporter.insert(s)
    }
    stats_exporter.send().unwrap()
}
