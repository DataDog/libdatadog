// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The generated registry matches `mappings.json`.

use libdd_trace_model::{PROMOTED_CHUNK_TAGS, PROMOTED_SPAN_TAGS};
use libdd_trace_semantics::{
    Concept, Predicate, Provider, ValueType, concept, concepts, content_hash, version,
};
use serde_json::Value;

fn mappings() -> Value {
    // Invalid JSON gives `null`, which fails the tests below.
    serde_json::from_str(include_str!("../mappings.json")).unwrap_or_default()
}

#[cfg_attr(miri, ignore = "no unsafe in crate; pure registry lookup logic")]
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

#[cfg_attr(miri, ignore = "no unsafe in crate; pure registry lookup logic")]
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
        let entries = c.entries();
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
                // `eq` implies presence, so `eq` with `present: true` is `Equals` too.
                let (present, eq) = (raw["present"].as_bool(), raw["eq"].as_str());
                match condition.predicate {
                    Predicate::Equals(value) => {
                        assert_eq!(Some(value), eq, "{name}");
                        assert_ne!(present, Some(false), "{name}");
                    }
                    Predicate::Present => assert_eq!((present, eq), (Some(true), None), "{name}"),
                    Predicate::Absent => assert_eq!((present, eq), (Some(false), None), "{name}"),
                    _ => panic!("{name}: unknown predicate {:?}", condition.predicate),
                }
            }
        }
    }
}

#[cfg_attr(miri, ignore = "no unsafe in crate; pure registry lookup logic")]
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

#[cfg_attr(miri, ignore = "no unsafe in crate; pure registry lookup logic")]
#[test]
fn http_method_prefers_otel_name() {
    let names: Vec<_> = Concept::HttpMethod
        .entries()
        .iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["http.request.method", "http.method"]);
}

/// Callers read promoted tags with their trait-model getters (e.g. `Span::span_kind`), not with
/// lookups, so a registry update that gives one of them another name would be silently missed.
#[cfg_attr(miri, ignore = "no unsafe in crate; pure registry lookup logic")]
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
        let names: Vec<_> = promoted.entries().iter().map(|e| e.name).collect();
        assert_eq!(names, [tag], "{action} Its entries are {names:?}.");
        assert!(
            promoted.entries().iter().all(|e| e.when.is_empty()),
            "promoted tag `{tag}` gained registry conditions; callers read it with its \
             trait-model getter, which ignores them. Decide how to handle it before merging this \
             registry update."
        );

        for &other in concepts() {
            for entry in other.entries() {
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
