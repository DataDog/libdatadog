// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The generated registry matches `mappings.json`.

use libdd_trace_model::{PROMOTED_CHUNK_TAGS, PROMOTED_SPAN_TAGS};
use libdd_trace_semantics::{
    Concept, Provider, ValueType, concept, concepts, content_hash, entries, version,
};
use serde_json::Value;

fn mappings() -> Value {
    // Invalid JSON gives `null`, which fails the tests below.
    serde_json::from_str(include_str!("../mappings.json")).unwrap_or_default()
}

#[test]
fn version_and_hash_come_from_the_json() {
    let json = mappings();
    assert_eq!(version(), json["version"].as_str().expect("version"));
    assert_eq!(
        content_hash(),
        json["metadata"]["content_hash"]
            .as_str()
            .expect("content_hash")
    );
}

#[test]
fn every_concept_is_generated_with_its_entries_in_order() {
    let json = mappings();
    let json_concepts = json["concepts"].as_object().expect("concepts");
    assert_eq!(concepts().len(), json_concepts.len());

    for (name, mapping) in json_concepts {
        let c = concept(name).unwrap_or_else(|| panic!("missing concept {name}"));
        assert_eq!(c.name(), name);
        assert!(concepts().contains(&c), "{name}");

        let fallbacks = mapping["fallbacks"].as_array().expect("fallbacks");
        let entries = entries(c);
        assert_eq!(entries.len(), fallbacks.len(), "{name}");
        for (entry, fallback) in entries.iter().zip(fallbacks) {
            assert_eq!(entry.name, fallback["name"], "{name}");
            assert_eq!(entry.version, fallback["version"].as_str(), "{name}");
            let provider = match entry.provider {
                Provider::Datadog => "datadog",
                Provider::Otel => "otel",
                _ => "unknown",
            };
            assert_eq!(provider, fallback["provider"], "{name}");
            let value_type = match entry.value_type {
                ValueType::String => "string",
                ValueType::Int64 => "int64",
                ValueType::Float64 => "float64",
                _ => "unknown",
            };
            assert_eq!(
                value_type,
                fallback["type"].as_str().unwrap_or("string"),
                "{name}"
            );

            let when = fallback["when"].as_array().map_or(&[][..], Vec::as_slice);
            assert_eq!(entry.when.len(), when.len(), "{name}");
            for (condition, raw) in entry.when.iter().zip(when) {
                assert_eq!(condition.attribute, raw["attribute"], "{name}");
                assert_eq!(condition.present, raw["present"].as_bool(), "{name}");
                assert_eq!(condition.eq, raw["eq"].as_str(), "{name}");
            }
        }
    }
}

#[test]
fn concept_by_name() {
    assert_eq!(concept("http.method"), Some(Concept::HttpMethod));
    assert_eq!(concept("_dd.top_level"), Some(Concept::DdTopLevel));
    assert_eq!(
        concept("peer.aws.s3.bucket"),
        Some(Concept::PeerAwsS3Bucket)
    );
    // Fallback names are not concepts.
    assert_eq!(concept("http.request.method"), None);
    assert_eq!(concept(""), None);
}

#[test]
fn http_method_prefers_otel_name() {
    let names: Vec<_> = entries(Concept::HttpMethod)
        .iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["http.request.method", "http.method"]);
}

/// Callers read promoted tags with their trait-model getters (e.g. `Span::span_kind`), not with
/// lookups, so a registry update that gives one of them another name would be silently missed.
#[test]
fn promoted_tags_have_no_other_names() {
    for tag in PROMOTED_SPAN_TAGS.into_iter().chain(PROMOTED_CHUNK_TAGS) {
        let action = format!(
            "promoted tag `{tag}` gained registry fallbacks (or is referenced elsewhere); callers \
             read it with its trait-model getter and would miss them. Decide how to handle it \
             before merging this registry update."
        );
        let promoted = concept(tag).unwrap_or_else(|| {
            panic!(
                "promoted tag `{tag}` is no longer a registry concept; callers read it with its \
                 trait-model getter. Decide how to handle it before merging this registry update."
            )
        });
        let names: Vec<_> = entries(promoted).iter().map(|e| e.name).collect();
        assert_eq!(names, [tag], "{action} Its entries are {names:?}.");
        assert!(
            entries(promoted).iter().all(|e| e.when.is_empty()),
            "promoted tag `{tag}` gained registry conditions; callers read it with its \
             trait-model getter, which ignores them. Decide how to handle it before merging this \
             registry update."
        );

        for &other in concepts() {
            for entry in entries(other) {
                assert!(
                    other == promoted || entry.name != tag,
                    "{action} Concept `{}` has an entry named `{tag}`.",
                    other.name()
                );
                assert!(
                    entry.when.iter().all(|c| c.attribute != tag),
                    "{action} Entry `{}` of concept `{}` has a condition on `{tag}`.",
                    entry.name,
                    other.name()
                );
            }
        }
    }
}
