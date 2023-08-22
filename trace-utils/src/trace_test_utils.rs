// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use std::collections::HashMap;

use datadog_trace_protobuf::pb;
use serde_json::json;

pub fn create_test_span(
    trace_id: u64,
    span_id: u64,
    parent_id: u64,
    start: i64,
    is_top_level: bool,
) -> pb::Span {
    let mut span = pb::Span {
        trace_id,
        span_id,
        service: "test-service".to_string(),
        name: "test_name".to_string(),
        resource: "test-resource".to_string(),
        parent_id,
        start,
        duration: 5,
        error: 0,
        meta: HashMap::from([
            ("service".to_string(), "test-service".to_string()),
            ("env".to_string(), "test-env".to_string()),
            (
                "runtime-id".to_string(),
                "afjksdljfkllksdj-28934889".to_string(),
            ),
        ]),
        metrics: HashMap::new(),
        r#type: "".to_string(),
        meta_struct: HashMap::new(),
    };
    if is_top_level {
        span.metrics.insert("_top_level".to_string(), 1.0);
        span.meta
            .insert("_dd.origin".to_string(), "cloudfunction".to_string());
        span.meta
            .insert("origin".to_string(), "cloudfunction".to_string());
        span.meta.insert(
            "functionname".to_string(),
            "dummy_function_name".to_string(),
        );
        span.r#type = "serverless".to_string();
    }
    span
}

pub fn create_test_json_span(
    trace_id: u64,
    span_id: u64,
    parent_id: u64,
    start: i64,
) -> serde_json::Value {
    json!(
        {
            "trace_id": trace_id,
            "span_id": span_id,
            "service": "test-service",
            "name": "test_name",
            "resource": "test-resource",
            "parent_id": parent_id,
            "start": start,
            "duration": 5,
            "error": 0,
            "meta": {
                "service": "test-service",
                "env": "test-env",
                "runtime-id": "afjksdljfkllksdj-28934889",
            },
            "metrics": {},
            "meta_struct": {},
        }
    )
}
