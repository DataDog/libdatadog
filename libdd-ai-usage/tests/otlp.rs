// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The OTLP encoding, decoded with the upstream OpenTelemetry message
//! definitions: every valid conformance observation is projected, batched and
//! encoded, and the decoded export must carry exactly the batch's points.

#![cfg(feature = "otlp")]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use libdd_ai_usage::{
    Instrument, Json, MetricBatch, OtlpScope, OtlpWindow, PROFILE_ATTRIBUTE, Profile,
    project_fixture_result,
};
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::common::v1::KeyValue;
use opentelemetry_proto::tonic::common::v1::any_value::Value;
use opentelemetry_proto::tonic::metrics::v1::AggregationTemporality;
use opentelemetry_proto::tonic::metrics::v1::metric::Data;
use opentelemetry_proto::tonic::metrics::v1::number_data_point;
use prost::Message;

const SCOPE: OtlpScope = OtlpScope {
    name: "libdd-ai-usage-test",
    version: "1.2.3",
};
const WINDOW: OtlpWindow = OtlpWindow {
    start_time_unix_nano: 1_791_000_000_000_000_000,
    time_unix_nano: 1_791_000_010_000_000_000,
};

fn data() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

/// Every valid case of every profile, in one batch.
fn batch() -> MetricBatch {
    let mut batch = MetricBatch::new();
    for dir in [
        "provider-attempt",
        "provider-streaming",
        "provider-token-breakdown",
        "gateway-request",
    ] {
        let mut paths: Vec<PathBuf> = fs::read_dir(data().join(dir).join("valid"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        paths.sort();
        for path in paths {
            let case = Json::parse(&fs::read_to_string(&path).unwrap()).unwrap();
            let profile =
                Profile::from_id(case.get("profile").and_then(Json::as_str).unwrap()).unwrap();
            batch
                .add(profile, &project_fixture_result(&case).unwrap())
                .unwrap();
        }
    }
    batch
}

fn attributes(values: &[KeyValue]) -> BTreeMap<String, String> {
    values
        .iter()
        .map(|kv| {
            let value = match kv.value.as_ref().and_then(|v| v.value.as_ref()) {
                Some(Value::StringValue(text)) => text.clone(),
                other => panic!("{} is not a string: {other:?}", kv.key),
            };
            (kv.key.clone(), value)
        })
        .collect()
}

type Key = (Option<String>, String, String, BTreeMap<String, String>);

#[test]
#[cfg_attr(
    miri,
    ignore = "projects every case; the encoder's own tests cover these paths under Miri"
)]
fn the_export_decodes_with_the_upstream_definitions_and_carries_every_point() {
    let batch = batch();
    let request = ExportMetricsServiceRequest::decode(batch.encode_otlp(SCOPE, WINDOW).as_slice())
        .expect("the export decodes");

    // What the batch holds: counter sums and histogram samples, per resource, profile and series.
    let mut expected_counters: BTreeMap<Key, f64> = BTreeMap::new();
    let mut expected_samples: BTreeMap<Key, Vec<f64>> = BTreeMap::new();
    for (resource, profile, points) in batch.points() {
        let resource = resource.get("service.name").cloned();
        for point in points {
            let key = (
                resource.clone(),
                profile.id().to_string(),
                point.name.clone(),
                point.attributes.clone(),
            );
            match point.instrument {
                Instrument::Counter => *expected_counters.entry(key).or_default() += point.value,
                Instrument::Histogram => expected_samples.entry(key).or_default().push(point.value),
            }
        }
    }

    let mut counters: BTreeMap<Key, f64> = BTreeMap::new();
    let mut histograms: BTreeMap<Key, (u64, f64, f64, f64)> = BTreeMap::new();
    for resource_metrics in &request.resource_metrics {
        let resource = resource_metrics
            .resource
            .as_ref()
            .map(|r| attributes(&r.attributes))
            .unwrap_or_default();
        assert!(!resource.contains_key(PROFILE_ATTRIBUTE));
        for scope_metrics in &resource_metrics.scope_metrics {
            let scope = scope_metrics.scope.as_ref().expect("a scope");
            assert_eq!(
                (scope.name.as_str(), scope.version.as_str()),
                ("libdd-ai-usage-test", "1.2.3")
            );
            let profile = attributes(&scope.attributes)
                .remove(PROFILE_ATTRIBUTE)
                .expect("the scope names its profile");
            let profile = Profile::from_id(&profile).expect("a known profile");
            for metric in &scope_metrics.metrics {
                assert!(
                    profile.metric_names().contains(&metric.name.as_str()),
                    "{}",
                    metric.name
                );
                match metric.data.as_ref().expect("metric data") {
                    Data::Sum(sum) => {
                        assert!(sum.is_monotonic);
                        assert_eq!(
                            sum.aggregation_temporality,
                            AggregationTemporality::Delta as i32
                        );
                        for point in &sum.data_points {
                            assert_eq!(point.start_time_unix_nano, WINDOW.start_time_unix_nano);
                            assert_eq!(point.time_unix_nano, WINDOW.time_unix_nano);
                            let value = match point.value {
                                Some(number_data_point::Value::AsInt(v)) => {
                                    // The values are counts and nano-USD: exact integers well below
                                    // 2^53.
                                    assert!(v >= 0);
                                    v.to_string().parse::<f64>().unwrap()
                                }
                                Some(number_data_point::Value::AsDouble(v)) => v,
                                None => panic!("a sum point without a value"),
                            };
                            let attrs = attributes(&point.attributes);
                            assert!(!attrs.contains_key(PROFILE_ATTRIBUTE));
                            let key = (
                                resource.get("service.name").cloned(),
                                profile.id().to_string(),
                                metric.name.clone(),
                                attrs,
                            );
                            assert!(
                                counters.insert(key, value).is_none(),
                                "one point per series"
                            );
                        }
                    }
                    Data::Histogram(histogram) => {
                        assert_eq!(
                            histogram.aggregation_temporality,
                            AggregationTemporality::Delta as i32
                        );
                        for point in &histogram.data_points {
                            assert_eq!(point.bucket_counts.len(), point.explicit_bounds.len() + 1);
                            assert_eq!(point.bucket_counts.iter().sum::<u64>(), point.count);
                            let attrs = attributes(&point.attributes);
                            let key = (
                                resource.get("service.name").cloned(),
                                profile.id().to_string(),
                                metric.name.clone(),
                                attrs,
                            );
                            let summary = (
                                point.count,
                                point.sum.expect("a sum"),
                                point.min.expect("a min"),
                                point.max.expect("a max"),
                            );
                            assert!(
                                histograms.insert(key, summary).is_none(),
                                "one point per series"
                            );
                        }
                    }
                    other => panic!("unexpected metric data {other:?}"),
                }
            }
        }
    }

    assert_eq!(counters, expected_counters);
    let expected_histograms: BTreeMap<Key, (u64, f64, f64, f64)> = expected_samples
        .into_iter()
        .map(|(key, samples)| {
            let count = u64::try_from(samples.len()).unwrap();
            let sum = samples.iter().sum();
            let min = samples.iter().copied().fold(f64::INFINITY, f64::min);
            let max = samples.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            (key, (count, sum, min, max))
        })
        .collect();
    assert_eq!(histograms, expected_histograms);
    assert!(counters.len() > 50 && histograms.len() > 50);
}

#[test]
fn a_point_that_states_its_profile_is_exported_once_under_the_scope() {
    let projection = |cost: &str| {
        let observation = Json::parse(&format!(
            r#"{{"operation_name": "chat", "provider_name": "openai", "duration_seconds": 1.5,
                "streaming": false, "input_tokens": 300, "output_tokens": 20,
                "cost_usd": {cost}, "cost_source": "estimated", "observation_point": "gateway"}}"#
        ))
        .unwrap();
        libdd_ai_usage::project_result(Profile::ProviderAttempt, &observation).unwrap()
    };
    let mut stated = projection("0.2");
    for point in &mut stated.points {
        point.attributes.insert(
            PROFILE_ATTRIBUTE.to_string(),
            Profile::ProviderAttempt.id().to_string(),
        );
    }
    let mut batch = MetricBatch::new();
    batch
        .add(Profile::ProviderAttempt, &projection("0.1"))
        .unwrap();
    batch.add(Profile::ProviderAttempt, &stated).unwrap();
    let request =
        ExportMetricsServiceRequest::decode(batch.encode_otlp(SCOPE, WINDOW).as_slice()).unwrap();
    let scope_metrics = &request.resource_metrics[0].scope_metrics[0];
    let scope = attributes(&scope_metrics.scope.as_ref().unwrap().attributes);
    assert_eq!(
        scope.get(PROFILE_ATTRIBUTE).map(String::as_str),
        Some(Profile::ProviderAttempt.id())
    );
    for metric in &scope_metrics.metrics {
        let points: Vec<&Vec<KeyValue>> = match &metric.data {
            Some(Data::Sum(sum)) => sum.data_points.iter().map(|p| &p.attributes).collect(),
            Some(Data::Histogram(h)) => h.data_points.iter().map(|p| &p.attributes).collect(),
            other => panic!("{}: {other:?}", metric.name),
        };
        for point in points {
            assert!(
                !attributes(point).contains_key(PROFILE_ATTRIBUTE),
                "{}",
                metric.name
            );
        }
    }
    let cost = scope_metrics
        .metrics
        .iter()
        .find(|m| m.name == "trajectory.gen_ai.client.inference.usage.cost")
        .unwrap();
    match &cost.data {
        Some(Data::Sum(sum)) => {
            assert_eq!(sum.data_points.len(), 1, "the two points are one series");
            assert_eq!(
                sum.data_points[0].value,
                Some(number_data_point::Value::AsInt(300_000_000))
            );
        }
        other => panic!("the cost is not a sum: {other:?}"),
    }
}

#[test]
fn bucket_bounds_follow_the_upstream_advice() {
    let mut batch = MetricBatch::new();
    let observation = Json::parse(
        r#"{"operation_name": "chat", "provider_name": "openai", "duration_seconds": 1.5,
            "streaming": true, "time_to_first_chunk_seconds": 0.3, "input_tokens": 300,
            "output_tokens": 20, "observation_point": "gateway"}"#,
    )
    .unwrap();
    batch
        .add(
            Profile::ProviderAttempt,
            &libdd_ai_usage::project_result(Profile::ProviderAttempt, &observation).unwrap(),
        )
        .unwrap();
    let request =
        ExportMetricsServiceRequest::decode(batch.encode_otlp(SCOPE, WINDOW).as_slice()).unwrap();
    let metrics = &request.resource_metrics[0].scope_metrics[0].metrics;
    let bounds = |name: &str| match &metrics.iter().find(|m| m.name == name).unwrap().data {
        Some(Data::Histogram(h)) => (
            h.data_points[0].explicit_bounds.clone(),
            h.data_points[0].bucket_counts.clone(),
        ),
        _ => panic!("{name} is not a histogram"),
    };
    let (duration, counts) = bounds("gen_ai.client.inference.duration");
    assert_eq!(duration.first(), Some(&0.01));
    assert_eq!(duration.last(), Some(&81.92));
    assert_eq!(counts[8], 1, "1.5 s is in (1.28, 2.56]");
    let (tokens, counts) = bounds("gen_ai.client.inference.operation.input_tokens");
    assert_eq!(tokens.last(), Some(&67108864.0));
    assert_eq!(counts[5], 1, "300 tokens is in (256, 1024]");
    let (first_chunk, _) = bounds("gen_ai.client.inference.time_to_first_chunk");
    assert_eq!(first_chunk.first(), Some(&0.001));
}
