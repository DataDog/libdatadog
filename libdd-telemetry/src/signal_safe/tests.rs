// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "signal-safe")]
mod transport;

use super::*;
use crate::data::{Application, Host};

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
