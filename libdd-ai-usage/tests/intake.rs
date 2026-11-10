// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A batch taken as metrics intake takes it: RATE points, sketches in
//! intake's `sketchpb.Sparse` form, the writer's tags and the timestamp
//! rule.

use libdd_ai_usage::{Json, MetricBatch, Profile, Sketch, project_result};
use prost::Message;

/// intake's `sketchpb.Basic` and `sketchpb.Sparse`, as `quantile.proto`
/// defines them.
#[derive(Clone, PartialEq, prost::Message)]
struct Basic {
    #[prost(double, tag = "1")]
    sum: f64,
    #[prost(double, tag = "2")]
    avg: f64,
    #[prost(int64, tag = "3")]
    count: i64,
    #[prost(double, tag = "4")]
    min: f64,
    #[prost(double, tag = "5")]
    max: f64,
}

#[derive(Clone, PartialEq, prost::Message)]
struct Sparse {
    #[prost(sint32, repeated, packed = "true", tag = "1")]
    k: Vec<i32>,
    #[prost(uint32, repeated, packed = "true", tag = "2")]
    n: Vec<u32>,
    #[prost(message, optional, tag = "3")]
    basic: Option<Basic>,
}

fn attempt(duration: f64) -> libdd_ai_usage::Projection {
    let observation = Json::parse(&format!(
        r#"{{"operation_name": "chat", "provider_name": "openai", "duration_seconds": {duration},
            "streaming": false, "input_tokens": 100, "input_basis": "includes_cache",
            "output_tokens": 20, "output_basis": "includes_reasoning",
            "observation_point": "gateway"}}"#
    ))
    .unwrap();
    project_result(Profile::ProviderAttempt, &observation).unwrap()
}

#[test]
fn sketches_decode_as_intake_sparse() {
    let sketch = Sketch::of(&[0.25, 1.5, 1.5, -3.0, 0.0]).unwrap();
    let decoded = Sparse::decode(sketch.encode_sparse().as_slice()).unwrap();
    assert_eq!(decoded.k, sketch.keys);
    assert_eq!(decoded.n, sketch.counts);
    let basic = decoded.basic.unwrap();
    assert_eq!(
        (basic.min, basic.max, basic.count, basic.sum, basic.avg),
        (-3.0, 1.5, 5, 0.25, 0.0)
    );
    // A negative sample, never a usage value, is counted as zero.
    assert!(decoded.k.contains(&0) && decoded.k.iter().all(|k| *k >= 0));
}

#[test]
fn a_take_is_rates_sketches_and_a_fresh_second() {
    let mut batch = MetricBatch::new();
    batch.set_writer("Pod-A", 42);
    for duration in [1.5, 0.5] {
        batch
            .add(Profile::ProviderAttempt, &attempt(duration))
            .unwrap();
    }
    let taken = batch.take_intake(1_000).unwrap();
    assert_eq!(
        (taken.timestamp, taken.interval, taken.dropped),
        (1_000, 10, 0)
    );
    assert!(batch.is_empty());
    let input = taken
        .rates
        .iter()
        .find(|s| s.metric == "gen_ai.client.inference.usage.input_tokens")
        .unwrap();
    // 200 tokens over 10 s.
    assert_eq!(input.value, 20.0);
    assert!(input.tags.contains(&"host.name:pod-a".to_string()));
    assert!(input.tags.contains(&"process.pid:42".to_string()));
    assert!(input.tags.windows(2).all(|w| w[0] <= w[1]));
    // A distribution's companions are rates too, and its samples one sketch.
    let count = taken
        .rates
        .iter()
        .find(|s| s.metric == "gen_ai.client.inference.duration.count")
        .unwrap();
    assert_eq!(count.value, 0.2);
    let durations: Vec<_> = taken
        .sketches
        .iter()
        .filter(|s| s.metric == "gen_ai.client.inference.duration")
        .collect();
    assert_eq!(durations.len(), 1);
    assert_eq!(durations[0].sketch.count, 2);
    assert_eq!(durations[0].sketch.sum, 2.0);

    // The next take never reuses a second, even when the clock has not moved.
    batch.add(Profile::ProviderAttempt, &attempt(1.0)).unwrap();
    assert_eq!(batch.take_intake(1_000).unwrap().timestamp, 1_001);
    assert_eq!(batch.take_intake(900).unwrap().timestamp, 1_002);
    assert_eq!(batch.take_intake(5_000).unwrap().timestamp, 5_000);
    // Drops since the last take.
    batch.set_max_len(Some(0));
    batch.add(Profile::ProviderAttempt, &attempt(1.0)).unwrap();
    let taken = batch.take_intake(6_000).unwrap();
    assert_eq!(
        (taken.dropped, taken.interval, taken.rates.len()),
        (1, 10, 0)
    );
}
