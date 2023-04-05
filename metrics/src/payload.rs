// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use ddcommon::tag::Tag;
use serde::Serialize;

use crate::metrics::Metric;

#[derive(Serialize)]
pub struct DistributionPayloadBuf {
    series: Vec<Metric>,
}

// construct_datadog_payload takes a vector of metrics and constructs the payload
// for the submit distribution metrics payload.
// docs: https://docs.datadoghq.com/api/latest/metrics/?code-lang=curl#submit-distribution-points
pub fn construct_distribution_payload(metrics: Vec<Metric>) -> Result<String, serde_json::Error> {
    #[derive(Serialize)]
    struct DistributionPayload {
        series: Vec<MetricPayload>,
    }

    #[derive(Serialize)]
    struct MetricPayload {
        metric: String,
        points: Vec<(u64, Vec<f64>)>,
        tags: Vec<Tag>,
    }

    let mut payloads: Vec<MetricPayload> = vec![];

    for metric in metrics {
        payloads.push(MetricPayload {
            metric: metric.metric,
            points: vec![(metric.points.timestamp, metric.points.values)],
            tags: metric.tags,
        })
    }

    serde_json::to_string(&DistributionPayload { series: payloads })
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::metrics::Points;
    use serde_json::json;

    #[test]
    fn test_construct_distribution_payload_empty() {
        let metrics = vec![];
        let payload = construct_distribution_payload(metrics).unwrap();
        let expected_payload = json!({ "series": [] }).to_string();
        assert_eq!(payload, expected_payload);
    }

    #[test]
    fn test_construct_distribution_payload_single_metric() {
        let metrics = vec![Metric::new(
            "test.metric".to_string(),
            Points::new(vec![42.0, 24.0], 1628695689),
            vec![
                Tag::new("env", "prod").unwrap(),
                Tag::new("service", "example").unwrap(),
            ],
        )];

        let payload = construct_distribution_payload(metrics).unwrap();
        let expected_payload = json!({
            "series": [
                {
                    "metric": "test.metric",
                    "points": [[1628695689, [42.0, 24.0]]],
                    "tags": ["env:prod", "service:example"],
                }
            ]
        })
        .to_string();

        assert_eq!(payload, expected_payload);
    }

    #[test]
    fn test_construct_distribution_payload_multiple_metrics() {
        let metrics = vec![
            Metric::new(
                "test.metric1".to_string(),
                Points::new(vec![42.0, 24.0], 1628695689),
                vec![Tag::new("env", "prod").unwrap()],
            ),
            Metric::new(
                "test.metric2".to_string(),
                Points::new(vec![12.0, 34.0], 1628695690),
                vec![Tag::new("service", "example").unwrap()],
            ),
        ];

        let payload = construct_distribution_payload(metrics).unwrap();
        let expected_payload = json!({
            "series": [
                {
                    "metric": "test.metric1",
                    "points": [[1628695689, [42.0, 24.0]]],
                    "tags": ["env:prod"],
                },
                {
                    "metric": "test.metric2",
                    "points": [[1628695690, [12.0, 34.0]]],
                    "tags": ["service:example"],
                }
            ]
        })
        .to_string();

        assert_eq!(payload, expected_payload);
    }
}
