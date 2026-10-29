// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{Criterion, criterion_group, criterion_main};
use libdd_ffe::rules_based::{
    Configuration, EvaluationContext, ExpectedFlagType, UniversalFlagConfig,
};
use serde_json::json;
use std::hint::black_box;

fn bench_dependencies(c: &mut Criterion) {
    let parent = json!({"key":"parent","enabled":true,"variationType":"BOOLEAN",
        "variations":{"on":{"key":"on","value":true}},
        "allocations":[{"key":"default","splits":[{"shards":[],"variationKey":"on"}]}]});
    for (name, repeated) in [
        ("legacy", 0),
        ("one_prerequisite", 1),
        ("memoized_16_references", 16),
    ] {
        let mut root = parent.clone();
        root["key"] = json!("root");
        if repeated > 0 {
            root["allocations"][0]["rules"] = json!([{"conditions": (0..repeated).map(|_|
                json!({"flagEvaluation":{"key":"parent"},"operator":"ONE_OF","value":["on"]})
            ).collect::<Vec<_>>()}]);
        }
        let config = Configuration::from_server_response(
            UniversalFlagConfig::from_json(
                serde_json::to_vec(
                    &json!({"createdAt":"2026-10-28T00:00:00Z","environment":{"name":"test"},
                "flags":{"root":root,"parent":parent}}),
                )
                .unwrap(),
            )
            .unwrap(),
        );
        let context = EvaluationContext::new(Some("subject".into()), Default::default());
        let now = chrono::Utc::now();
        c.bench_function(&format!("dependent_flags/{name}"), |b| {
            b.iter(|| {
                black_box(config.eval_flag(
                    black_box("root"),
                    black_box(&context),
                    ExpectedFlagType::Boolean,
                    now,
                ))
                .unwrap()
            })
        });
    }
}
criterion_group!(benches, bench_dependencies);
criterion_main!(benches);
