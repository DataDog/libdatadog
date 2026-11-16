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
/// Maximum byte length of the canonical features object. Features are kept in
/// sorted key order up to the last one that fits.
pub const MAX_FEATURES_BYTES: usize = 4096;

/// Parse a flat JSON object of features, keeping only string, number and
/// boolean values. Numbers become doubles, so `1` and `1.0` are the same value
/// while `1` and `"1"` are not. Empty, invalid or non-object input yields no
/// features. The result encodes to at most [`MAX_FEATURES_BYTES`].
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
    let mut features = BTreeMap::new();
    // Starts at 2 for the enclosing braces.
    let mut encoded_len = 2;
    for (key, value) in parsed
        .into_iter()
        .filter(|(key, _)| within_text_cap(key))
        .filter_map(|(key, value)| scalar(value).map(|value| (key, value)))
        .take(MAX_FEATURES)
    {
        let separator = usize::from(!features.is_empty());
        let entry_len = encoded_entry_len(&key, &value);
        if encoded_len + separator + entry_len > MAX_FEATURES_BYTES {
            log::debug!("ffe features exceed {MAX_FEATURES_BYTES} bytes; truncating");
            break;
        }
        encoded_len += separator + entry_len;
        features.insert(key, value);
    }
    features
}

fn encoded_entry_len(key: &str, value: &Value) -> usize {
    let json_len = |value: &Value| serde_json::to_string(value).map_or(usize::MAX, |s| s.len());
    // `"key":value`; the colon is the extra byte.
    json_len(&Value::from(key))
        .saturating_add(1)
        .saturating_add(json_len(value))
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
            // -0.0 == 0.0, but it serializes with its sign and would split buckets.
            .map(|value| if value == 0.0 { 0.0 } else { value })
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

fn within_text_cap(text: &str) -> bool {
    text.chars().take(MAX_FEATURE_TEXT_LENGTH + 1).count() <= MAX_FEATURE_TEXT_LENGTH
}

#[cfg(test)]
/// Features whose canonical encoding is exactly `len` bytes.
pub(crate) fn features_of_encoded_len(len: usize) -> String {
    // A full entry `"kNNN":"<100 x>"` is 109 bytes; entries after the first add a comma.
    const ENTRY: usize = 109;
    let mut features = BTreeMap::new();
    let mut encoded = 2;
    while encoded + 2 * (ENTRY + 1) <= len {
        encoded += usize::from(!features.is_empty()) + ENTRY;
        features.insert(
            format!("k{:03}", features.len()),
            serde_json::json!("x".repeat(100)),
        );
    }
    let separator = usize::from(!features.is_empty());
    let last_value = len - encoded - separator - r#""kNNN":"""#.len();
    features.insert(
        format!("k{:03}", features.len()),
        serde_json::json!("y".repeat(last_value)),
    );
    let raw = serde_json::to_string(&features).unwrap();
    assert_eq!(raw.len(), len);
    raw
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
    fn features_are_kept_up_to_the_byte_cap() {
        let at_cap = features_of_encoded_len(MAX_FEATURES_BYTES);
        assert_eq!(
            canonical_features_json(&at_cap).as_deref(),
            Some(at_cap.as_str())
        );

        let over_cap = features_of_encoded_len(MAX_FEATURES_BYTES + 1);
        let truncated = canonical_features_json(&over_cap).unwrap();
        let mut expected: BTreeMap<String, Value> = serde_json::from_str(&over_cap).unwrap();
        expected.pop_last();
        assert_eq!(truncated, serde_json::to_string(&expected).unwrap());
        assert!(truncated.len() <= MAX_FEATURES_BYTES);
    }

    #[test]
    fn truncation_keeps_a_sorted_key_prefix() {
        let mut features: BTreeMap<String, Value> =
            serde_json::from_str(&features_of_encoded_len(MAX_FEATURES_BYTES - 20)).unwrap();
        let kept = features.clone();
        features.insert("zz_large".to_owned(), json!("x".repeat(100)));
        features.insert("zzz".to_owned(), json!(1));
        let raw = serde_json::to_string(&features).unwrap();
        assert_eq!(features_map(&raw), kept);
    }

    #[test]
    fn byte_cap_counts_escaped_text() {
        let escaped = r"\u0001".repeat(MAX_FEATURE_TEXT_LENGTH);
        let raw = format!(
            "{{{}}}",
            (0..10)
                .map(|i| format!(r#""k{i}":"{escaped}""#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let canonical = canonical_features_json(&raw).unwrap();
        assert!(canonical.len() <= MAX_FEATURES_BYTES, "{}", canonical.len());
        assert_eq!(features_map(&raw).len(), 2);
    }

    #[test]
    fn negative_zero_encodes_like_zero() {
        assert_eq!(
            canonical_features_json(r#"{"a":-0.0}"#),
            canonical_features_json(r#"{"a":0}"#)
        );
        assert_eq!(
            canonical_features_json(r#"{"a":-0.0}"#).unwrap(),
            r#"{"a":0.0}"#
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
