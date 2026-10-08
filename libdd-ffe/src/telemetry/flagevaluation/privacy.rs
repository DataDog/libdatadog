// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Privacy operations shared by aggregation and the independent output guard.

use super::{MAX_CONTEXT_DEPTH, MAX_CONTEXT_FIELDS, MAX_FIELD_LENGTH};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Finite context-loss reasons shared with the other FFE SDKs.
#[derive(Clone, Copy, Debug, strum_macros::EnumIter, strum_macros::EnumCount)]
#[repr(usize)]
pub enum ContextTruncationReason {
    /// Top-level context width or the shared retained-leaf budget was exceeded.
    MaxContextFields,
    MaxKeyLength,
    MaxValueLength,
    MaxListElements,
    /// A nested object's property limit was exceeded.
    MaxStructureProperties,
    MaxSnapshotDepth,
    MaxVisitedNodes,
    SnapshotError,
}

impl ContextTruncationReason {
    /// Number of counters, derived from the enum variants.
    pub const COUNT: usize = <Self as strum::EnumCount>::COUNT;

    /// Every possible reason, in counter order.
    pub fn iter() -> impl Iterator<Item = Self> {
        <Self as strum::IntoEnumIterator>::iter()
    }

    /// Stable telemetry tag; never contains customer-controlled text.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MaxContextFields => "max_context_fields",
            Self::MaxKeyLength => "max_key_length",
            Self::MaxValueLength => "max_value_length",
            Self::MaxListElements => "max_list_elements",
            Self::MaxStructureProperties => "max_structure_properties",
            Self::MaxSnapshotDepth => "max_snapshot_depth",
            Self::MaxVisitedNodes => "max_visited_nodes",
            Self::SnapshotError => "snapshot_error",
        }
    }
}

/// Internal field-loss metadata. Each reason is counted once per observation,
/// even when normalization runs at more than one boundary. Never part of EVP.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FieldOmissions {
    context_reasons: u16,
    /// Invalid UTF-8 identity was omitted rather than replacing its bytes.
    pub targeting_key_invalid: bool,
}

impl FieldOmissions {
    /// Record a context omission without retaining its input or error text.
    pub fn record_context(&mut self, reason: ContextTruncationReason) {
        self.context_reasons |= 1 << (reason as usize);
    }

    /// Whether this observation incurred a particular context loss.
    pub fn contains_context(self, reason: ContextTruncationReason) -> bool {
        self.context_reasons & (1 << (reason as usize)) != 0
    }
}

fn within_character_cap(value: &str) -> bool {
    value.chars().take(MAX_FIELD_LENGTH + 1).count() <= MAX_FIELD_LENGTH
}

/// Legacy input is already JSON: parsing still visits its bytes. Only the
/// subsequent snapshot traversal/retention is bounded. The PHP hot path must
/// select bounded input before creating this representation.
pub(super) fn context_json(raw: &str, omissions: &mut FieldOmissions) -> Option<String> {
    context_value(raw, omissions).map(|value| value.to_string())
}

pub(super) fn context_value(raw: &str, omissions: &mut FieldOmissions) -> Option<Value> {
    let Ok(Value::Object(attrs)) = serde_json::from_str::<Value>(raw) else {
        omissions.record_context(ContextTruncationReason::SnapshotError);
        return None;
    };
    let mut snapshot = Snapshot::new(omissions);
    Some(Value::Object(snapshot.object(attrs.iter(), attrs.len(), 1)))
}

pub(super) fn context_map(attrs: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    let mut omissions = FieldOmissions::default();
    Snapshot::new(&mut omissions)
        .object(attrs.iter(), attrs.len(), 1)
        .into_iter()
        .collect()
}

/// Builds a bounded copy of a context, sharing budgets across all nested values.
/// Retained scalars and empty containers consume the leaf budget; inspected
/// entries (including rejected ones) consume the visited-node budget. Omission
/// reasons are recorded without retaining the rejected data.
struct Snapshot<'a> {
    omissions: &'a mut FieldOmissions,
    leaves: usize,
    visited: usize,
}

impl<'a> Snapshot<'a> {
    /// Start fresh traversal budgets while preserving any prior omission reasons.
    fn new(omissions: &'a mut FieldOmissions) -> Self {
        Self {
            omissions,
            leaves: 0,
            visited: 0,
        }
    }

    /// Reserve one inspected entry, or record the exhausted budget and stop.
    fn visit(&mut self) -> bool {
        if self.leaves >= MAX_CONTEXT_FIELDS {
            self.omissions
                .record_context(ContextTruncationReason::MaxContextFields);
            return false;
        }
        // Like Python, cap rejected branches too: a leaf cap alone does not
        // bound traversal of broad trees whose leaves are all invalid.
        if self.visited >= MAX_CONTEXT_FIELDS * (MAX_CONTEXT_DEPTH + 1) {
            self.omissions
                .record_context(ContextTruncationReason::MaxVisitedNodes);
            return false;
        }
        self.visited += 1;
        true
    }

    /// Inspect only the bounded prefix in the map's existing order. Rejected
    /// entries still consume that prefix; later entries cannot replace them.
    /// `depth` counts this object, with the root object at depth one.
    fn object<'v>(
        &mut self,
        entries: impl Iterator<Item = (&'v String, &'v Value)>,
        len: usize,
        depth: usize,
    ) -> Map<String, Value> {
        let mut result = Map::new();
        if len > MAX_CONTEXT_FIELDS {
            self.omissions.record_context(if depth == 1 {
                ContextTruncationReason::MaxContextFields
            } else {
                ContextTruncationReason::MaxStructureProperties
            });
        }
        // Map/BTreeMap already have stable iteration; never collect/sort an
        // unbounded set of keys or search past the inspected prefix for a fit.
        for (key, value) in entries.take(MAX_CONTEXT_FIELDS) {
            if !self.visit() {
                break;
            }
            if !within_character_cap(key) {
                self.omissions
                    .record_context(ContextTruncationReason::MaxKeyLength);
                continue;
            }
            if let Some(value) = self.value(value, depth) {
                result.insert(key.clone(), value);
            }
        }
        result
    }

    /// Copy an admitted value at its containing object's/array's depth. Oversized
    /// strings and too-deep containers are omitted, not truncated. Nested
    /// containers share the same budgets and are omitted if pruning empties them.
    fn value(&mut self, value: &Value, depth: usize) -> Option<Value> {
        match value {
            Value::String(s) if !within_character_cap(s) => {
                self.omissions
                    .record_context(ContextTruncationReason::MaxValueLength);
                None
            }
            Value::Object(attrs) if !attrs.is_empty() => {
                if depth >= MAX_CONTEXT_DEPTH {
                    self.omissions
                        .record_context(ContextTruncationReason::MaxSnapshotDepth);
                    return None;
                }
                let result = self.object(attrs.iter(), attrs.len(), depth + 1);
                (!result.is_empty()).then_some(Value::Object(result))
            }
            Value::Array(values) if !values.is_empty() => {
                if depth >= MAX_CONTEXT_DEPTH {
                    self.omissions
                        .record_context(ContextTruncationReason::MaxSnapshotDepth);
                    return None;
                }
                if values.len() > MAX_CONTEXT_FIELDS {
                    self.omissions
                        .record_context(ContextTruncationReason::MaxListElements);
                }
                let mut result = Vec::new();
                for value in values.iter().take(MAX_CONTEXT_FIELDS) {
                    if !self.visit() {
                        break;
                    }
                    if let Some(value) = self.value(value, depth + 1) {
                        result.push(value);
                    }
                }
                (!result.is_empty()).then_some(Value::Array(result))
            }
            _ => {
                self.leaves += 1;
                Some(value.clone())
            }
        }
    }
}

pub(super) fn protected_targeting_key(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    format!("sha256_{:x}", Sha256::digest(value.as_bytes()))
}

/// OpenFeature error codes only: evaluator text can contain customer data even
/// when the customer consented to collecting evaluation context.
/// Accept exact canonical codes only; alternate casing, whitespace, and arbitrary
/// evaluator messages become GENERAL rather than being treated as known codes.
pub(super) fn error_code(value: &str) -> &'static str {
    match value {
        "PROVIDER_NOT_READY" => "PROVIDER_NOT_READY",
        "PROVIDER_FATAL" => "PROVIDER_FATAL",
        "FLAG_NOT_FOUND" => "FLAG_NOT_FOUND",
        "PARSE_ERROR" => "PARSE_ERROR",
        "TYPE_MISMATCH" => "TYPE_MISMATCH",
        "TARGETING_KEY_MISSING" => "TARGETING_KEY_MISSING",
        "INVALID_CONTEXT" => "INVALID_CONTEXT",
        _ => "GENERAL",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn error_codes_accept_only_canonical_openfeature_values() {
        for code in [
            "PROVIDER_NOT_READY",
            "PROVIDER_FATAL",
            "FLAG_NOT_FOUND",
            "PARSE_ERROR",
            "TYPE_MISMATCH",
            "TARGETING_KEY_MISSING",
            "INVALID_CONTEXT",
            "GENERAL",
        ] {
            assert_eq!(error_code(code), code);
        }
        for code in [
            "flag_not_found",
            "FlagNotFound",
            " FLAG_NOT_FOUND ",
            "private-error-canary",
        ] {
            assert_eq!(error_code(code), "GENERAL");
        }
    }

    #[test]
    fn invalid_json_omits_only_context_and_has_a_finite_reason() {
        for raw in ["{private-canary", "null", "[]", "1"] {
            let mut omissions = FieldOmissions::default();
            assert_eq!(context_json(raw, &mut omissions), None);
            assert!(omissions.contains_context(ContextTruncationReason::SnapshotError));
            assert!(!format!("{omissions:?}").contains("private-canary"));
        }
    }

    #[test]
    fn root_and_nested_object_widths_report_distinct_reasons() {
        let attrs: Map<String, Value> = (0..257)
            .map(|i| (format!("field{i:03}"), json!("kept")))
            .collect();
        for nested in [false, true] {
            let raw = if nested {
                json!({"nested": attrs})
            } else {
                json!(attrs)
            };
            let mut omissions = FieldOmissions::default();
            let retained = context_value(&raw.to_string(), &mut omissions).unwrap();
            let fields = if nested {
                &retained["nested"]
            } else {
                &retained
            };
            assert_eq!(fields.as_object().unwrap().len(), 256);
            let mut expected = FieldOmissions::default();
            expected.record_context(if nested {
                ContextTruncationReason::MaxStructureProperties
            } else {
                ContextTruncationReason::MaxContextFields
            });
            assert_eq!(omissions, expected);
        }
    }

    #[test]
    fn container_caps_and_depth_bound_retained_values() {
        let raw = json!({"a": (0..257).collect::<Vec<_>>()}).to_string();
        let mut omissions = FieldOmissions::default();
        let retained = context_json(&raw, &mut omissions).unwrap();
        let value: Value = serde_json::from_str(&retained).unwrap();
        assert_eq!(value["a"].as_array().unwrap().len(), 256);
        assert!(omissions.contains_context(ContextTruncationReason::MaxListElements));
        assert_eq!(context_json(&raw, &mut omissions).unwrap(), retained);
        let raw = json!({"a": {"b": {"c": {"d": 1, "deep": {"lost": 2}}}}}).to_string();
        let value: Value =
            serde_json::from_str(&context_json(&raw, &mut omissions).unwrap()).unwrap();
        assert_eq!(value["a"]["b"]["c"]["d"], 1);
        assert!(value["a"]["b"]["c"].get("deep").is_none());
        assert!(omissions.contains_context(ContextTruncationReason::MaxSnapshotDepth));
    }

    #[test]
    fn sibling_arrays_share_the_leaf_budget_after_repeated_normalization() {
        let raw = json!({
            "a": (0..200).collect::<Vec<_>>(),
            "b": (200..400).collect::<Vec<_>>(),
        })
        .to_string();
        let mut omissions = FieldOmissions::default();
        let retained = context_json(&raw, &mut omissions).unwrap();
        let value: Value = serde_json::from_str(&retained).unwrap();
        assert_eq!(value["a"], json!((0..200).collect::<Vec<_>>()));
        assert_eq!(value["b"], json!((200..256).collect::<Vec<_>>()));
        assert!(omissions.contains_context(ContextTruncationReason::MaxContextFields));
        assert!(!omissions.contains_context(ContextTruncationReason::MaxListElements));
        let original_omissions = omissions;
        for _ in 0..2 {
            assert_eq!(context_json(&retained, &mut omissions).unwrap(), retained);
            assert_eq!(omissions, original_omissions);
        }
    }

    #[test]
    fn rejected_fields_consume_width_and_global_visit_budgets() {
        let mut attrs = Map::new();
        for i in 0..257 {
            attrs.insert(format!("k{i:03}"), json!("x".repeat(257)));
        }
        let root = json!({"a": attrs.clone(), "b": attrs.clone(), "c": attrs.clone(),
            "d": attrs.clone(), "e": attrs, "z": "unvisited"});
        let mut omissions = FieldOmissions::default();
        assert_eq!(
            context_json(&root.to_string(), &mut omissions).as_deref(),
            Some("{}")
        );
        assert!(omissions.contains_context(ContextTruncationReason::MaxVisitedNodes));
        assert!(omissions.contains_context(ContextTruncationReason::MaxStructureProperties));
        assert!(omissions.contains_context(ContextTruncationReason::MaxValueLength));
    }

    #[test]
    fn null_false_and_empty_values_survive_snapshot() {
        let raw = r#"{"a":null,"b":false,"c":"","d":{},"e":[]}"#;
        let mut omissions = FieldOmissions::default();
        assert_eq!(context_json(raw, &mut omissions).as_deref(), Some(raw));
        assert_eq!(omissions, FieldOmissions::default());
    }
}
