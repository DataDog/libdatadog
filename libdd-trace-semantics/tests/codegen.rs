// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The validation `build.rs` applies to `mappings.json`.

#[path = "../codegen.rs"]
#[allow(dead_code)]
mod codegen;

use codegen::{Error, parse, variant_name};

fn registry(concepts: &str) -> String {
    format!(r#"{{"version":"v1","metadata":{{"content_hash":"h"}},"concepts":{{{concepts}}}}}"#)
}

const HTTP_METHOD: &str = r#""http.method":{"canonical":"http.method","fallbacks":[{"name":"http.method","provider":"otel","type":"string"}]}"#;

fn rejected(json: &str) -> String {
    match parse(json) {
        Ok(_) => "accepted".to_owned(),
        Err(Error::Invalid(msg)) => msg,
        Err(Error::Json(e)) => e.to_string(),
    }
}

#[test]
fn accepts_the_embedded_file() {
    let registry = parse(include_str!("../mappings.json")).expect("valid");
    assert!(!registry.concepts.is_empty());
}

#[test]
fn accepts_a_minimal_registry() {
    let registry = parse(&registry(HTTP_METHOD)).expect("valid");
    assert_eq!(registry.version, "v1");
    assert_eq!(registry.content_hash, "h");
    assert_eq!(registry.concepts[0].variant, "HttpMethod");
}

#[test]
fn missing_type_is_a_string() {
    let json = registry(r#""a":{"canonical":"a","fallbacks":[{"name":"a","provider":"datadog"}]}"#);
    let registry = parse(&json).expect("valid");
    assert_eq!(
        registry.concepts[0].entries[0].value_type,
        codegen::ValueType::String
    );
}

#[test]
fn unknown_top_level_and_metadata_fields_are_allowed() {
    let json = format!(
        r#"{{"version":"v1","extra":1,"metadata":{{"content_hash":"h","timestamp":"t"}},"concepts":{{{HTTP_METHOD}}}}}"#
    );
    parse(&json).expect("valid");
}

#[test]
fn rejects_invalid_registries() {
    let cases = [
        (
            r#"{"metadata":{"content_hash":"h"},"concepts":{}}"#.to_owned(),
            "missing field `version`",
        ),
        (
            r#"{"version":"","metadata":{"content_hash":"h"},"concepts":{"a":{"canonical":"a","fallbacks":[{"name":"a","provider":"otel"}]}}}"#.to_owned(),
            "`version` is empty",
        ),
        (
            r#"{"version":"v1","metadata":{},"concepts":{}}"#.to_owned(),
            "missing field `content_hash`",
        ),
        (
            r#"{"version":"v1","metadata":{"content_hash":" "},"concepts":{}}"#.to_owned(),
            "`metadata.content_hash` is empty",
        ),
        (registry(""), "there are no concepts"),
        (
            registry(&format!("{HTTP_METHOD},{HTTP_METHOD}")),
            "duplicate concept `http.method`",
        ),
        (
            registry(r#""a":{"canonical":"b","fallbacks":[{"name":"a","provider":"otel"}]}"#),
            "concept `a` has canonical name `b`",
        ),
        (
            registry(r#""a":{"canonical":"a","fallbacks":[]}"#),
            "concept `a` has no fallbacks",
        ),
        (
            registry(r#""a":{"canonical":"a","fallbacks":[{"name":"a","provider":"otel"}],"x":1}"#),
            "unknown field `x`",
        ),
        (
            registry(r#""a":{"canonical":"a","fallbacks":[{"name":"","provider":"otel"}]}"#),
            "has a fallback with no name",
        ),
        (
            registry(r#""a":{"canonical":"a","fallbacks":[{"name":"a","provider":"aws"}]}"#),
            "unknown variant `aws`",
        ),
        (
            registry(r#""a":{"canonical":"a","fallbacks":[{"name":"a","provider":"otel","type":"bool"}]}"#),
            "unknown variant `bool`",
        ),
        (
            registry(r#""a":{"canonical":"a","fallbacks":[{"name":"a","provider":"otel","version":""}]}"#),
            "has an empty version",
        ),
        (
            registry(r#""a":{"canonical":"a","fallbacks":[{"name":"a","provider":"otel","priority":1}]}"#),
            "unknown field `priority`",
        ),
        (
            registry(r#""a":{"canonical":"a","fallbacks":[{"name":"a","provider":"otel","when":[{"attribute":"b"}]}]}"#),
            "with neither `eq` nor `present`",
        ),
        (
            registry(r#""a":{"canonical":"a","fallbacks":[{"name":"a","provider":"otel","when":[{"attribute":"","eq":"x"}]}]}"#),
            "has a condition with no attribute",
        ),
        (
            registry(r#""a":{"canonical":"a","fallbacks":[{"name":"a","provider":"otel","when":[{"attribute":"b","neq":"x"}]}]}"#),
            "unknown field `neq`",
        ),
        (
            registry(r#""a":{"canonical":"a","fallbacks":[{"name":"a","provider":"otel","when":[{"attribute":"b","eq":""}]}]}"#),
            "with an empty `eq`",
        ),
        (
            registry(r#""a":{"canonical":"a","fallbacks":[{"name":"a","provider":"otel","when":[{"attribute":"b","eq":"x","present":false}]}]}"#),
            "with both `eq` and `present: false`",
        ),
        (
            registry(r#""self":{"canonical":"self","fallbacks":[{"name":"a","provider":"otel"}]}"#),
            "does not give a valid Rust name",
        ),
        (
            registry(r#""a.b":{"canonical":"a.b","fallbacks":[{"name":"a","provider":"otel"}]},"a_b":{"canonical":"a_b","fallbacks":[{"name":"a","provider":"otel"}]}"#),
            "concepts `a.b` and `a_b` both map to the Rust name `AB`",
        ),
    ];
    for (json, want) in cases {
        let msg = rejected(&json);
        assert!(msg.contains(want), "{json}: got `{msg}`, want `{want}`");
    }
}

#[test]
fn variant_names() {
    for (concept, want) in [
        ("http.method", "HttpMethod"),
        ("_dd.top_level", "DdTopLevel"),
        ("peer.aws.s3.bucket", "PeerAwsS3Bucket"),
        ("_sampling_priority_v1", "SamplingPriorityV1"),
        ("k8s.pod.uid", "K8sPodUid"),
        ("a-b", "AB"),
    ] {
        assert_eq!(variant_name(concept).expect("valid"), want);
    }
    for concept in ["", "1a", "_.", "a b", "a/b", "é", "self", "_self"] {
        assert!(variant_name(concept).is_err(), "{concept}");
    }
}
