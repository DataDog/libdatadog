// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::json_scanner::{Op, Scanner};
use crate::obfuscation_config::{JsonObfuscatorConfig, JsonStringTransformer};

/// Obfuscates a JSON string by replacing all leaf values with `"?"`, unless the value
/// belongs to a key listed in `keep_keys`, in which case it is left verbatim.
/// Keys in `transform_keys` have their string values passed through a transformer function
/// (e.g. SQL obfuscation) instead of being replaced with `"?"`.
///
/// Multiple concatenated JSON objects in the input are each obfuscated independently.
/// On a parse error the output so far is returned with `"..."` appended.
pub struct JsonObfuscator {
    config: JsonObfuscatorConfig,
}

enum ClosureKind {
    Array,
    Object,
}

impl JsonObfuscator {
    pub fn new(config: JsonObfuscatorConfig) -> Self {
        Self { config }
    }

    /// Obfuscates json string and return an optional error on malformatted json
    /// If an error occurs, an value is returned anyways which might be truncated (...)
    pub fn obfuscate(&self, input: &str) -> (String, Option<String>) {
        if input.is_empty() {
            return (String::new(), None);
        }

        let mut out = String::with_capacity(input.len());
        let mut scanner = Scanner::new();
        let mut buf = String::new(); // accumulates key chars or transform-value chars
        let mut closures: Vec<ClosureKind> = Vec::new();
        let mut keep_depth: usize = 0;
        let mut key = false;
        let mut wiped = false;
        let mut keeping = false;
        let mut transforming_value = false;

        for c in input.chars() {
            let op = scanner.step(c);
            let depth = closures.len(); // snapshot before any mutation

            match op {
                Op::BeginObject => {
                    closures.push(ClosureKind::Object);
                    set_key(&closures, &mut key, &mut wiped);
                    transforming_value = false;
                }
                Op::BeginArray => {
                    closures.push(ClosureKind::Array);
                    set_key(&closures, &mut key, &mut wiped);
                    transforming_value = false;
                }
                Op::EndArray | Op::EndObject => {
                    closures.pop();
                    set_key(&closures, &mut key, &mut wiped);
                    handle_value_done(
                        &mut out,
                        &mut buf,
                        &mut keeping,
                        &mut transforming_value,
                        &mut keep_depth,
                        depth,
                        self.config.transformer.as_ref(),
                    );
                }
                Op::ObjectValue | Op::ArrayValue => {
                    set_key(&closures, &mut key, &mut wiped);
                    handle_value_done(
                        &mut out,
                        &mut buf,
                        &mut keeping,
                        &mut transforming_value,
                        &mut keep_depth,
                        depth,
                        self.config.transformer.as_ref(),
                    );
                }
                Op::BeginLiteral | Op::Continue => {
                    if transforming_value {
                        buf.push(c);
                        continue;
                    } else if key {
                        buf.push(c);
                    } else if !keeping {
                        if !wiped {
                            out.push_str("\"?\"");
                            wiped = true;
                        }
                        continue;
                    }
                }
                Op::ObjectKey => {
                    let k = buf.trim_matches('"');
                    if !keeping && self.config.keep_keys.contains(k) {
                        keeping = true;
                        keep_depth = depth + 1;
                    } else if !transforming_value
                        && self.config.transformer.is_some()
                        && self.config.transform_keys.contains(k)
                    {
                        transforming_value = true;
                    }
                    buf.clear();
                    key = false;
                }
                Op::SkipSpace => continue,
                Op::Error => {
                    out.push_str("...");
                    return (out, scanner.err);
                }
                Op::End => {} // whitespace between JSON objects — fall through to output char
            }

            out.push(c);
        }

        if scanner.eof() == Op::Error {
            out.push_str("...");
        }
        (out, scanner.err)
    }
}

/// Updates `key` and `wiped` based on the current closure stack.
/// `key` is true at top level or when inside an object (not an array).
fn set_key(closures: &[ClosureKind], key: &mut bool, wiped: &mut bool) {
    let n = closures.len();
    *key = n == 0 || matches!(closures[n - 1], ClosureKind::Object);
    *wiped = false;
}

/// Handles the "value is done" logic after a value-ending opcode.
/// Writes the transformer result if applicable, or stops keeping if depth shrinks.
fn handle_value_done(
    out: &mut String,
    buf: &mut String,
    keeping: &mut bool,
    transforming_value: &mut bool,
    keep_depth: &mut usize,
    depth: usize,
    transformer: Option<&JsonStringTransformer>,
) {
    if *transforming_value {
        if let Some(t) = transformer {
            // Unquote the collected JSON string literal (handles escape sequences).
            let raw: String =
                serde_json::from_str(buf).unwrap_or_else(|_| buf.trim_matches('"').to_string());
            let result = t(&raw);
            out.push('"');
            out.push_str(&result);
            out.push('"');
            *transforming_value = false;
            buf.clear();
        }
    } else if *keeping && depth < *keep_depth {
        *keeping = false;
    }
}

#[cfg(test)]
mod tests {
    use duplicate::duplicate_item;
    use serde_json::json;

    use super::JsonObfuscator;
    use crate::{obfuscation_config::JsonObfuscatorConfig, sql::obfuscate_sql_string};

    fn obf(keep_keys: &[&str]) -> JsonObfuscator {
        JsonObfuscator::new(JsonObfuscatorConfig {
            enabled: true,
            keep_keys: keep_keys.iter().map(|key| key.to_string()).collect(),
            ..Default::default()
        })
    }

    fn obf_sql(keep_keys: &[&str], transform_keys: &[&str]) -> JsonObfuscator {
        JsonObfuscator::new(JsonObfuscatorConfig {
            enabled: true,
            keep_keys: keep_keys.iter().map(|s| s.to_string()).collect(),
            transform_keys: transform_keys.iter().map(|s| s.to_string()).collect(),
            transformer: Some(obfuscate_sql_string),
        })
    }

    fn assert_json_eq(result: &str, expected: &str) {
        let result: serde_json::Value =
            serde_json::from_str(result).expect("result is not valid JSON");
        let expected: serde_json::Value =
            serde_json::from_str(expected).expect("expected is not valid JSON");
        assert_eq!(result, expected);
    }

    // Basic obfuscation tests — parametric over (keep_keys, input, expected).
    // Uses assert_json_eq (structural comparison, whitespace-insensitive).
    #[duplicate_item(
        test_name                         keep_keys           input                                                                                                                          expected;
        [test_empty_object]               [&[]]               ["{}"]                                                                                                                         ["{}"];
        [test_empty_array]                [&[]]               ["[]"]                                                                                                                         ["[]"];
        [test_emoji_object]                [&["🐵"]]               [r#"{"🐵":"🙊"}"#]                                                                                                                         [r#"{"🐵":"🙊"}"#];
        [test_nested_empty_objects]       [&[]]               [r#"{"a":{},"b":{"c":{}}}"#]                                                                                                  [r#"{"a":{},"b":{"c":{}}}"#];
        [test_boolean_and_null_obfuscated][&[]]               [r#"{"a":true,"b":false,"c":null}"#]                                                                                          [r#"{"a":"?","b":"?","c":"?"}"#];
        [test_all_values_obfuscated]      [&[]]               [r#"{"query":{"multi_match":{"query":"guide","fields":["_all",{"key":"value","other":["1","2",{"k":"v"}]},"2"]}}}"#]           [r#"{"query":{"multi_match":{"query":"?","fields":["?",{"key":"?","other":["?","?",{"k":"?"}]},"?"]}}}"#];
        [test_numbers_obfuscated]         [&[]]               [r#"{"highlight":{"pre_tags":["<em>"],"post_tags":["</em>"],"index":1}}"#]                                                    [r#"{"highlight":{"pre_tags":["?"],"post_tags":["?"],"index":"?"}}"#];
        [test_keep_key_keeps_entire_value][&["other"]]        [r#"{"query":{"multi_match":{"query":"guide","fields":["_all",{"key":"value","other":["1","2",{"k":"v"}]},"2"]}}}"#]           [r#"{"query":{"multi_match":{"query":"?","fields":["?",{"key":"?","other":["1","2",{"k":"v"}]},"?"]}}}"#];
        [test_keep_key_nested_array_fully_kept][&["fields"]]  [r#"{"fields":["_all",{"key":"value","other":["1","2",{"k":"v"}]},"2"]}"#]                                                    [r#"{"fields":["_all",{"key":"value","other":["1","2",{"k":"v"}]},"2"]}"#];
        [test_keep_key_deep_nested]       [&["k"]]            [r#"{"fields":["_all",{"key":"value","other":["1","2",{"k":"v"}]},"2"]}"#]                                                    [r#"{"fields":["?",{"key":"?","other":["?","?",{"k":"v"}]},"?"]}"#];
        [test_keep_key_in_nested_object]  [&["C"]]            [r#"{"fields":[{"A":1,"B":{"C":3}},"2"]}"#]                                                                                   [r#"{"fields":[{"A":"?","B":{"C":3}},"?"]}"#];
        [test_keep_key_large_nested_structure][&["hits"]]     [r#"{"outer":{"total":2,"max_score":0.9105287,"hits":[{"_index":"bookdb_index","_score":0.9105287}]}}"#]                      [r#"{"outer":{"total":"?","max_score":"?","hits":[{"_index":"bookdb_index","_score":0.9105287}]}}"#];
        [test_keep_multiple_keys]         [&["_index","title"]][r#"{"hits":[{"_index":"bookdb_index","_type":"book","_score":0.9,"_source":{"summary":"text","title":"ES in Action","publish_date":"2015-12-03"},"highlight":{"title":["ES Action"]}}]}"#] [r#"{"hits":[{"_index":"bookdb_index","_type":"?","_score":"?","_source":{"summary":"?","title":"ES in Action","publish_date":"?"},"highlight":{"title":["ES Action"]}}]}"#];
        [test_keep_key_wallet]            [&["company_wallet_configuration_id"]] [r#"{"email":"dev@datadoghq.com","company_wallet_configuration_id":1}"#] [r#"{"email":"?","company_wallet_configuration_id":1}"#];
    )]
    #[test]
    fn test_name() {
        let (res, err) = obf(keep_keys).obfuscate(input);
        assert_eq!(err, None);
        assert_json_eq(&res, expected);
    }

    // Truncation / error tests — parametric over (input, expected_exact_string).
    #[duplicate_item(
        test_name                           input                                                                    expected                                          expected_error;
        [test_empty_input]                  [""]                                                                     [""]                                              [None];
        [test_invalid_emoji]                ["🤨"]                                                                   ["..."]                                           [Some("invalid character '🤨' looking for beginning of value".to_owned())];
        [test_invalid_unicode]              ["ჸ"]                                                                    ["..."]                                           [Some("invalid character 'ჸ' looking for beginning of value".to_owned())];
        [test_invalid_json_appends_ellipsis]["INVALID"]                                                              ["..."]                                           [Some("invalid character 'I' looking for beginning of value".to_owned())];
        [test_invalid_single_char]          [")"]                                                                    ["..."]                                           [Some("invalid character ')' looking for beginning of value".to_owned())];
        [test_truncated_open_value_string]  [r#"{"query":""#]                                                        [r#"{"query":"?"..."#]                            [Some("unexpected end of JSON input at char position 11".to_owned())];
        [test_truncated_multi_json]         [r#"{"first json": "valid"} {"second json": "unfinished"#]               [r#"{"first json":"?"} {"second json":"?"..."#]   [Some("unexpected end of JSON input at char position 53".to_owned())];
    )]
    #[test]
    fn test_name() {
        let (res, err) = obf(&[]).obfuscate(input);
        assert_eq!(res, expected);
        assert_eq!(err, expected_error);
    }

    #[test]
    fn test_multiple_json_objects() {
        // Multiple concatenated JSON objects (elasticsearch bulk API pattern).
        let input = r#"{"index":{"_index":"traces","_type":"trace"}} {"value":1,"name":"test"}"#;
        let (result, err) = obf(&[]).obfuscate(input);
        assert_eq!(err, None);
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
    fn test_transform_key_sql_basic() {
        let input = r#"{"query":"select * from table where id = 2","hello":"world","hi":"there"}"#;
        let (result, err) = obf_sql(&["hello"], &["query"]).obfuscate(input);
        assert_eq!(err, None);

        let val: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(val["hello"], json!("world"));
        assert_eq!(val["hi"], json!("?"));
        assert!(
            val["query"].as_str().unwrap().contains('?'),
            "SQL value should be obfuscated"
        );
    }

    #[test]
    fn test_transform_key_with_object_value_falls_through() {
        let input = r#"{"object":{"not a":"query"}}"#;
        let expected = r#"{"object":{"not a":"?"}}"#;
        let (res, err) = obf_sql(&[], &["object"]).obfuscate(input);
        assert_eq!(err, None);

        assert_json_eq(&res, expected);
    }

    #[test]
    fn test_transform_key_with_array_value_falls_through() {
        let input = r#"{"object":["not","a","query"]}"#;
        let expected = r#"{"object":["?","?","?"]}"#;
        let (res, err) = obf_sql(&[], &["object"]).obfuscate(input);
        assert_eq!(err, None);

        assert_json_eq(&res, expected);
    }
}
