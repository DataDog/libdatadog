// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;

use serde_json::Value;

type Transformer = Box<dyn Fn(&str) -> String + Send + Sync>;

/// Obfuscates a JSON string by replacing all leaf values with `"?"`, unless the value
/// belongs to a key listed in `keep_keys`, in which case it is left verbatim.
/// Keys in `transform_keys` have their string values passed through a transformer function
/// (e.g. SQL obfuscation) instead of being replaced with `"?"`.
///
/// Multiple concatenated JSON objects in the input are each obfuscated independently.
/// On a parse error the output so far is returned with `"..."` appended.
pub struct JsonObfuscator {
    keep_keys: HashSet<String>,
    transform_keys: HashSet<String>,
    transformer: Option<Transformer>,
}

impl JsonObfuscator {
    pub fn new(
        keep_keys: impl IntoIterator<Item = String>,
        transform_keys: impl IntoIterator<Item = String>,
        transformer: Option<Transformer>,
    ) -> Self {
        Self {
            keep_keys: keep_keys.into_iter().collect(),
            transform_keys: transform_keys.into_iter().collect(),
            transformer,
        }
    }

    pub fn obfuscate(&self, input: &str) -> String {
        if input.is_empty() {
            return String::new();
        }

        let mut out = String::with_capacity(input.len());
        let stream = serde_json::Deserializer::from_str(input).into_iter::<Value>();

        for result in stream {
            match result {
                Ok(value) => out.push_str(
                    &serde_json::to_string(&self.obfuscate_value(value)).unwrap_or_default(),
                ),
                Err(_) => {
                    out.push_str("...");
                    break;
                }
            }
        }

        out
    }

    fn obfuscate_value(&self, value: Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(k, v)| {
                        let v = self.obfuscate_entry(&k, v);
                        (k, v)
                    })
                    .collect(),
            ),
            Value::Array(arr) => {
                Value::Array(arr.into_iter().map(|v| self.obfuscate_value(v)).collect())
            }
            _ => Value::String("?".to_string()),
        }
    }

    fn obfuscate_entry(&self, key: &str, value: Value) -> Value {
        if self.keep_keys.contains(key) {
            return value;
        }
        if let Some(transformer) = &self.transformer {
            if self.transform_keys.contains(key) {
                return match value {
                    Value::String(s) => Value::String(transformer(&s)),
                    other => self.obfuscate_value(other),
                };
            }
        }
        self.obfuscate_value(value)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::JsonObfuscator;
    use crate::sql::obfuscate_sql_string;

    fn obf(keep_keys: &[&str]) -> JsonObfuscator {
        JsonObfuscator::new(keep_keys.iter().map(|s| s.to_string()), [], None)
    }

    fn obf_sql(keep_keys: &[&str], transform_keys: &[&str]) -> JsonObfuscator {
        JsonObfuscator::new(
            keep_keys.iter().map(|s| s.to_string()),
            transform_keys.iter().map(|s| s.to_string()),
            Some(Box::new(obfuscate_sql_string)),
        )
    }

    fn assert_json_eq(result: &str, expected: &str) {
        let result: serde_json::Value =
            serde_json::from_str(result).expect("result is not valid JSON");
        let expected: serde_json::Value =
            serde_json::from_str(expected).expect("expected is not valid JSON");
        assert_eq!(result, expected);
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(obf(&[]).obfuscate(""), "");
    }

    #[test]
    fn test_all_values_obfuscated() {
        // elasticsearch.body.1
        let input = r#"{"query":{"multi_match":{"query":"guide","fields":["_all",{"key":"value","other":["1","2",{"k":"v"}]},"2"]}}}"#;
        let expected = r#"{"query":{"multi_match":{"query":"?","fields":["?",{"key":"?","other":["?","?",{"k":"?"}]},"?"]}}}"#;
        assert_json_eq(&obf(&[]).obfuscate(input), expected);
    }

    #[test]
    fn test_numbers_obfuscated() {
        // elasticsearch.body.2
        let input = r#"{"highlight":{"pre_tags":["<em>"],"post_tags":["</em>"],"index":1}}"#;
        let expected = r#"{"highlight":{"pre_tags":["?"],"post_tags":["?"],"index":"?"}}"#;
        assert_json_eq(&obf(&[]).obfuscate(input), expected);
    }

    #[test]
    fn test_keep_key_keeps_entire_value() {
        // elasticsearch.body.3: keep "other" preserves the whole array value
        let input = r#"{"query":{"multi_match":{"query":"guide","fields":["_all",{"key":"value","other":["1","2",{"k":"v"}]},"2"]}}}"#;
        let expected = r#"{"query":{"multi_match":{"query":"?","fields":["?",{"key":"?","other":["1","2",{"k":"v"}]},"?"]}}}"#;
        assert_json_eq(&obf(&["other"]).obfuscate(input), expected);
    }

    #[test]
    fn test_keep_key_nested_array_fully_kept() {
        // elasticsearch.body.4: keep "fields" keeps the entire array
        let input = r#"{"fields":["_all",{"key":"value","other":["1","2",{"k":"v"}]},"2"]}"#;
        let expected = r#"{"fields":["_all",{"key":"value","other":["1","2",{"k":"v"}]},"2"]}"#;
        assert_json_eq(&obf(&["fields"]).obfuscate(input), expected);
    }

    #[test]
    fn test_keep_key_deep_nested() {
        // elasticsearch.body.5: keep "k" only at the exact key occurrence
        let input = r#"{"fields":["_all",{"key":"value","other":["1","2",{"k":"v"}]},"2"]}"#;
        let expected = r#"{"fields":["?",{"key":"?","other":["?","?",{"k":"v"}]},"?"]}"#;
        assert_json_eq(&obf(&["k"]).obfuscate(input), expected);
    }

    #[test]
    fn test_keep_key_in_nested_object() {
        // elasticsearch.body.6: keep "C" inside nested object
        let input = r#"{"fields":[{"A":1,"B":{"C":3}},"2"]}"#;
        let expected = r#"{"fields":[{"A":"?","B":{"C":3}},"?"]}"#;
        assert_json_eq(&obf(&["C"]).obfuscate(input), expected);
    }

    #[test]
    fn test_keep_key_large_nested_structure() {
        // elasticsearch.body.11: keep "hits" preserves its entire value
        let input = r#"{"outer":{"total":2,"max_score":0.9105287,"hits":[{"_index":"bookdb_index","_score":0.9105287}]}}"#;
        let expected = r#"{"outer":{"total":"?","max_score":"?","hits":[{"_index":"bookdb_index","_score":0.9105287}]}}"#;
        assert_json_eq(&obf(&["hits"]).obfuscate(input), expected);
    }

    #[test]
    fn test_keep_multiple_keys() {
        // elasticsearch.body.12: keep "_index" and "title" individually
        let input = r#"{"hits":[{"_index":"bookdb_index","_type":"book","_score":0.9,"_source":{"summary":"text","title":"ES in Action","publish_date":"2015-12-03"},"highlight":{"title":["ES Action"]}}]}"#;
        let expected = r#"{"hits":[{"_index":"bookdb_index","_type":"?","_score":"?","_source":{"summary":"?","title":"ES in Action","publish_date":"?"},"highlight":{"title":["ES Action"]}}]}"#;
        assert_json_eq(&obf(&["_index", "title"]).obfuscate(input), expected);
    }

    #[test]
    fn test_keep_key_wallet() {
        // obfuscate.mongo.json.keep_values
        let input = r#"{"email":"dev@datadoghq.com","company_wallet_configuration_id":1}"#;
        let expected = r#"{"email":"?","company_wallet_configuration_id":1}"#;
        assert_json_eq(
            &obf(&["company_wallet_configuration_id"]).obfuscate(input),
            expected,
        );
    }

    #[test]
    fn test_multiple_json_objects() {
        // Multiple concatenated JSON objects (elasticsearch bulk API pattern).
        // The output is also concatenated — parse each value out of the stream.
        let input = r#"{"index":{"_index":"traces","_type":"trace"}} {"value":1,"name":"test"}"#;
        let result = obf(&[]).obfuscate(input);
        let mut stream =
            serde_json::Deserializer::from_str(&result).into_iter::<serde_json::Value>();
        let first = stream
            .next()
            .expect("first value")
            .expect("first value is valid JSON");
        let second = stream
            .next()
            .expect("second value")
            .expect("second value is valid JSON");
        assert_eq!(first, json!({"index":{"_index":"?","_type":"?"}}));
        assert_eq!(second, json!({"value":"?","name":"?"}));
    }

    #[test]
    fn test_invalid_json_appends_ellipsis() {
        let result = obf(&[]).obfuscate("INVALID");
        assert_eq!(result, "...");
    }

    #[test]
    fn test_partial_json_appends_ellipsis() {
        // A truncated JSON object — partial output + "..."
        let result = obf(&[]).obfuscate(r#"{"key": "value""#);
        assert!(
            result.ends_with("..."),
            "expected '...' suffix, got: {result}"
        );
    }

    #[test]
    fn test_transform_key_sql_basic() {
        // obfuscate.sql.json.basic
        let input = r#"{"query":"select * from table where id = 2","hello":"world","hi":"there"}"#;
        let result = obf_sql(&["hello"], &["query"]).obfuscate(input);
        let val: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(val["hello"], json!("world"));
        assert_eq!(val["hi"], json!("?"));
        // SQL obfuscated: literal 2 → ?
        assert!(
            val["query"].as_str().unwrap().contains('?'),
            "SQL value should be obfuscated"
        );
    }

    #[test]
    fn test_transform_key_with_object_value_falls_through() {
        // obfuscate.sql.json.tried_sql_obfuscate_an_object
        let input = r#"{"object":{"not a":"query"}}"#;
        let expected = r#"{"object":{"not a":"?"}}"#;
        assert_json_eq(&obf_sql(&[], &["object"]).obfuscate(input), expected);
    }

    #[test]
    fn test_transform_key_with_array_value_falls_through() {
        // obfuscate.sql.json.tried_sql_obfuscate_an_array
        let input = r#"{"object":["not","a","query"]}"#;
        let expected = r#"{"object":["?","?","?"]}"#;
        assert_json_eq(&obf_sql(&[], &["object"]).obfuscate(input), expected);
    }

    #[test]
    fn test_empty_object() {
        assert_json_eq(&obf(&[]).obfuscate("{}"), "{}");
    }

    #[test]
    fn test_empty_array() {
        assert_json_eq(&obf(&[]).obfuscate("[]"), "[]");
    }

    #[test]
    fn test_nested_empty_objects() {
        let input = r#"{"a":{},"b":{"c":{}}}"#;
        let expected = r#"{"a":{},"b":{"c":{}}}"#;
        assert_json_eq(&obf(&[]).obfuscate(input), expected);
    }

    #[test]
    fn test_boolean_and_null_obfuscated() {
        let input = r#"{"a":true,"b":false,"c":null}"#;
        let expected = r#"{"a":"?","b":"?","c":"?"}"#;
        assert_json_eq(&obf(&[]).obfuscate(input), expected);
    }
}
