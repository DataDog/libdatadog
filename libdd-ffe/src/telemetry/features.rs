// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Split features attached to exposure events and flag evaluation counts.
//!
//! Producers route features per destination and send a flat JSON object of
//! scalar values. Parsing is lenient: malformed input never rejects the event
//! that carries it.

use serde_json::Value;
use std::collections::BTreeMap;

/// Maximum number of features retained per event, in sorted key order.
pub const MAX_FEATURES: usize = 256;
/// Maximum Unicode character count of a feature key or string value. Longer
/// entries are skipped, not truncated.
pub const MAX_FEATURE_TEXT_LENGTH: usize = 256;

/// Parse a flat JSON object of features, keeping only string, number and
/// boolean values. Numbers become doubles, so `1` and `1.0` are the same value
/// while `1` and `"1"` are not. Empty, invalid or non-object input yields no
/// features.
pub fn features_map(raw: &str) -> BTreeMap<String, Value> {
    if raw.is_empty() {
        return BTreeMap::new();
    }
    let parsed = match serde_json::from_str::<BTreeMap<String, Value>>(raw) {
        Ok(parsed) => parsed,
        Err(_) => {
            // serde errors can quote the rejected input, which is customer data.
            log::debug!("ffe features must be a JSON object; omitting features");
            return BTreeMap::new();
        }
    };
    parsed
        .into_iter()
        .filter(|(key, _)| within_text_cap(key))
        .filter_map(|(key, value)| scalar(value).map(|value| (key, value)))
        .take(MAX_FEATURES)
        .collect()
}

/// Canonical encoding of [`features_map`]: a JSON object with sorted keys, or
/// `None` when no feature survives. Equal feature sets encode identically.
pub fn canonical_features_json(raw: &str) -> Option<String> {
    let features = features_map(raw);
    if features.is_empty() {
        return None;
    }
    serde_json::to_string(&features).ok()
}

fn scalar(value: Value) -> Option<Value> {
    match value {
        Value::String(text) => within_text_cap(&text).then_some(Value::String(text)),
        Value::Bool(_) => Some(value),
        Value::Number(number) => number
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

fn within_text_cap(text: &str) -> bool {
    text.chars().take(MAX_FEATURE_TEXT_LENGTH + 1).count() <= MAX_FEATURE_TEXT_LENGTH
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keeps_only_scalar_values() {
        let features = features_map(
            r#"{"s":"a","n":2,"b":false,"null":null,"list":[1],"obj":{"a":1},"empty":""}"#,
        );
        assert_eq!(
            serde_json::to_value(&features).unwrap(),
            json!({"s":"a","n":2.0,"b":false,"empty":""})
        );
    }

    #[test]
    fn rejected_input_is_not_logged() {
        crate::telemetry::test_logging::start_log_capture();
        for raw in [
            r#""FEATURE_CANARY""#,
            r#"{"FEATURE_CANARY":"#,
            r#"["FEATURE_CANARY"]"#,
        ] {
            assert!(features_map(raw).is_empty());
        }
        let logs = crate::telemetry::test_logging::captured_logs();
        assert_eq!(logs.len(), 3);
        assert!(logs.iter().all(|log| log.level == log::Level::Debug
            && log.message == "ffe features must be a JSON object; omitting features"));
    }

    #[test]
    fn malformed_input_yields_no_features() {
        for raw in ["", "not json", "[]", "\"text\"", "1", "null", "{}"] {
            assert!(features_map(raw).is_empty(), "{raw}");
            assert_eq!(canonical_features_json(raw), None, "{raw}");
        }
    }

    #[test]
    fn canonical_encoding_is_sorted_and_type_tagged() {
        assert_eq!(
            canonical_features_json(r#"{"b":1,"a":"1"}"#),
            canonical_features_json(r#"{"a":"1","b":1.0}"#)
        );
        assert_eq!(
            canonical_features_json(r#"{"b":1,"a":"1"}"#).unwrap(),
            r#"{"a":"1","b":1.0}"#
        );
        assert_ne!(
            canonical_features_json(r#"{"a":1}"#),
            canonical_features_json(r#"{"a":"1"}"#)
        );
        assert_ne!(
            canonical_features_json(r#"{"a":true}"#),
            canonical_features_json(r#"{"a":"true"}"#)
        );
    }

    #[test]
    fn overlong_text_is_skipped_not_truncated() {
        let exact = "é".repeat(MAX_FEATURE_TEXT_LENGTH);
        let over = "é".repeat(MAX_FEATURE_TEXT_LENGTH + 1);
        let raw = json!({ &exact: &exact, &over: "v", "long": &over }).to_string();
        let features = features_map(&raw);
        assert_eq!(features.len(), 1);
        assert_eq!(features[&exact], json!(exact));
    }

    #[test]
    fn retains_at_most_max_features_in_key_order() {
        let raw = serde_json::to_string(
            &(0..=MAX_FEATURES)
                .map(|i| (format!("k{i:03}"), json!(i)))
                .collect::<BTreeMap<_, _>>(),
        )
        .unwrap();
        let features = features_map(&raw);
        assert_eq!(features.len(), MAX_FEATURES);
        assert!(features.contains_key("k000"));
        assert!(!features.contains_key(&format!("k{MAX_FEATURES:03}")));
    }
}
