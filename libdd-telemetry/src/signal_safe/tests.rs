// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "signal-safe")]
mod transport;

use super::*;
#[cfg(feature = "std")]
use crate::data::{
    ApiVersion, GenerateMetrics, Payload, Telemetry,
    metrics::{Serie, Tag},
};
use crate::data::{Application, Host};

#[cfg(feature = "std")]
#[test]
fn encoding_matches_the_owned_serde_model() {
    let tags = [
        TagRef::new("component", "signal-safe").unwrap(),
        TagRef::new("escaped", "\"yes\"\n").unwrap(),
    ];
    let points = [(1_234, 1.5), (1_235, 2.0)];
    let series = [MetricSeriesRef {
        namespace: MetricNamespace::Telemetry,
        metric: "submission.count",
        points: &points,
        tags: &tags,
        common: false,
        metric_type: MetricType::Count,
        interval: 10,
    }];
    let application = Application {
        service_name: "service\nname".to_string(),
        service_version: Some("1.2.3".to_string()),
        env: Some("staging".to_string()),
        language_name: "rust".to_string(),
        language_version: "1.87".to_string(),
        tracer_version: "35.0.0".to_string(),
        runtime_name: Some("rustc".to_string()),
        runtime_version: Some("1.87".to_string()),
        runtime_patches: Some("0".to_string()),
        process_tags: Some("region:us-east-1".to_string()),
    };
    let host = Host {
        hostname: "host".to_string(),
        container_id: Some("container".to_string()),
        os: Some("linux".to_string()),
        os_version: Some("1".to_string()),
        architecture: Some("x86_64".to_string()),
        kernel_name: Some("Linux".to_string()),
        kernel_release: Some("6".to_string()),
        kernel_version: Some("6.1".to_string()),
    };
    let request = MetricsRequest {
        tracer_time: 1_236,
        runtime_id: "runtime-id",
        seq_id: 7,
        application: &application,
        host: &host,
        origin: Some("library"),
        series: &series,
    };
    let payload = Payload::GenerateMetrics(GenerateMetrics {
        series: vec![Serie {
            namespace: MetricNamespace::Telemetry,
            metric: "submission.count".to_string(),
            points: points.to_vec(),
            tags: vec![
                Tag::new("component", "signal-safe").unwrap(),
                Tag::new("escaped", "\"yes\"\n").unwrap(),
            ],
            common: false,
            _type: MetricType::Count,
            interval: 10,
        }],
    });
    let expected = Telemetry {
        api_version: ApiVersion::V2,
        tracer_time: 1_236,
        runtime_id: "runtime-id",
        seq_id: 7,
        application: &application,
        host: &host,
        origin: Some("library"),
        payload: &payload,
    };

    let mut buffer = [0_u8; 2_048];
    let len = encode_metrics(&request, &mut buffer).unwrap();
    let actual: serde_json::Value = serde_json::from_slice(&buffer[..len]).unwrap();

    assert_eq!(actual, serde_json::to_value(expected).unwrap());
}

#[test]
fn reports_a_small_buffer() {
    let application = Application::default();
    let host = Host::default();
    let request = MetricsRequest {
        tracer_time: 1,
        runtime_id: "runtime",
        seq_id: 1,
        application: &application,
        host: &host,
        origin: None,
        series: &[],
    };
    let mut buffer = [0_u8; 16];

    assert!(matches!(
        encode_metrics(&request, &mut buffer),
        Err(Error::BufferTooSmall)
    ));
}

#[test]
fn rejects_non_finite_values() {
    let application = Application::default();
    let host = Host::default();
    let points = [(1, f64::NAN)];
    let series = [MetricSeriesRef {
        namespace: MetricNamespace::Telemetry,
        metric: "submission.count",
        points: &points,
        tags: &[],
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
    let mut buffer = [0_u8; 1_024];

    assert!(matches!(
        encode_metrics(&request, &mut buffer),
        Err(Error::NonFiniteValue)
    ));
}
