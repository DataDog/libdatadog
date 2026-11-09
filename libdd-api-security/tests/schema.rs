// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![allow(clippy::unwrap_used, clippy::expect_used)]

use libdd_api_security::{
    MAX_ARRAY_NODES, MAX_CONTAINER_DEPTH, MAX_RECORD_NODES, SchemaError, extract_schema,
    extract_schema_json,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

fn output(input: &Value) -> Value {
    let schema = serde_json::to_value(extract_schema(input)).unwrap();
    let json = serde_json::to_string(input).unwrap();
    let streamed: Value = serde_json::from_str(&extract_schema_json(&json).unwrap()).unwrap();
    assert_eq!(schema, streamed, "parsed and streaming entry points");
    schema
}

// libddwaf's array variants are an unordered set. Normalize only schema
// variants (not the outer node/metadata sequence) when comparing fixtures.
fn canonicalize(schema: &mut Value) {
    let parts = schema.as_array_mut().expect("schema node");
    match &mut parts[0] {
        Value::Array(variants) => {
            for variant in variants.iter_mut() {
                canonicalize(variant);
            }
            variants.sort_by_cached_key(|value| serde_json::to_string(value).unwrap());
        }
        Value::Object(fields) => {
            for child in fields.values_mut() {
                canonicalize(child);
            }
            fields.sort_keys();
        }
        _ => {}
    }
}

#[derive(Deserialize)]
struct Fixture {
    name: String,
    input: String,
    schema: Value,
}

fn check_fixture(name: &str) {
    let fixtures: Vec<Fixture> =
        serde_json::from_str(include_str!("fixtures/structural.json")).unwrap();
    let mut fixture = fixtures
        .into_iter()
        .find(|fixture| fixture.name == name)
        .unwrap();
    let input: Value = serde_json::from_str(&fixture.input).unwrap();
    let mut actual = output(&input);
    let mut from_json: Value =
        serde_json::from_str(&extract_schema_json(&fixture.input).unwrap()).unwrap();
    canonicalize(&mut actual);
    canonicalize(&mut from_json);
    canonicalize(&mut fixture.schema);
    assert_eq!(actual, fixture.schema, "parsed input: {name}");
    assert_eq!(from_json, fixture.schema, "JSON boundary: {name}");
}

macro_rules! fixture_tests {
    ($($name:ident),+ $(,)?) => {$(
        #[test]
        fn $name() {
            check_fixture(stringify!($name));
        }
    )+};
}

fixture_tests!(
    null,
    boolean_true,
    boolean_false,
    zero,
    negative_integer,
    signed_min,
    signed_max,
    unsigned_max,
    float,
    integral_float,
    negative_zero,
    exponent,
    empty_string,
    string,
    unicode_string,
    empty_array,
    empty_record,
    heterogeneous_scalars,
    duplicate_scalars,
    signed_unsigned_collapse,
    integer_float_distinction,
    duplicate_records,
    duplicate_arrays,
    duplicate_containers,
    record,
    empty_keys,
    reordered_records,
    reordered_nested_arrays,
    different_nested_lengths,
    empty_container_types,
    first_ten_elements,
    nested_record_array_order,
    no_scanner_metadata,
);

fn wide_record(size: usize) -> Value {
    Value::Object(
        (0..size)
            .map(|i| (format!("key-{i:03}"), json!(i)))
            .collect(),
    )
}

fn array_chain(value: Value, containers: usize) -> Value {
    (0..containers).fold(value, |child, _| Value::Array(vec![child]))
}

fn wrap_array_schemas(value: Value, containers: usize) -> Value {
    (0..containers).fold(value, |child, _| json!([[child], {"len": 1}]))
}

#[test]
fn array_limit_boundaries() {
    for len in [0, 1, 9, 10, 11, 1000] {
        let input = Value::Array(vec![json!("value"); len]);
        let actual = output(&input);
        let variants = actual[0].as_array().unwrap();
        assert_eq!(variants.len(), usize::from(len > 0), "len={len}");
        assert_eq!(actual[1]["len"], json!(len));
        assert_eq!(actual[1].get("truncated").is_some(), len > MAX_ARRAY_NODES);
    }
}

#[test]
fn record_limit_boundaries() {
    for len in [0, 1, 254, 255, 256, 1000] {
        let actual = output(&wide_record(len));
        assert_eq!(
            actual[0].as_object().unwrap().len(),
            len.min(MAX_RECORD_NODES)
        );
        assert_eq!(
            actual.as_array().unwrap().len(),
            1 + usize::from(len > MAX_RECORD_NODES)
        );
        if len > MAX_RECORD_NODES {
            assert_eq!(actual[1], json!({"truncated": true}));
        }
    }
}

#[test]
fn json_record_selection_precedes_sorting() {
    let fields: Vec<_> = (0..=MAX_RECORD_NODES)
        .rev()
        .map(|i| format!(r#""key-{i:03}":{i}"#))
        .collect();
    let input = format!("{{{}}}", fields.join(","));
    let actual: Value = serde_json::from_str(&extract_schema_json(&input).unwrap()).unwrap();
    let selected = actual[0].as_object().unwrap();
    assert_eq!(selected.len(), MAX_RECORD_NODES);
    assert!(!selected.contains_key("key-000"));
    assert!(selected.contains_key("key-255"));
    assert_eq!(selected.keys().next().unwrap(), "key-001");
}

#[test]
fn parsed_value_selection_uses_supplied_map_order() {
    let fields: Map<_, _> = (0..=MAX_RECORD_NODES)
        .rev()
        .map(|i| (format!("key-{i:03}"), json!(i)))
        .collect();
    let expected_keys: Vec<_> = fields.keys().take(MAX_RECORD_NODES).cloned().collect();
    let actual = output(&Value::Object(fields));
    for key in expected_keys {
        assert!(actual[0].get(&key).is_some());
    }
    assert_eq!(actual[0].as_object().unwrap().len(), MAX_RECORD_NODES);
}

#[test]
fn empty_keys_consume_record_quota() {
    let mut fields = Map::new();
    fields.insert(String::new(), json!("skip"));
    for i in 0..MAX_RECORD_NODES {
        fields.insert(format!("key-{i:03}"), json!(true));
    }
    let actual = output(&Value::Object(fields));
    assert_eq!(actual[0].as_object().unwrap().len(), MAX_RECORD_NODES - 1);
    assert!(actual[0].get("key-253").is_some());
    assert!(actual[0].get("key-254").is_none());
    assert_eq!(actual[1], json!({"truncated": true}));
}

#[test]
fn depth_limit_array_boundaries() {
    for depth in [16, 17, 18, 19, 100] {
        let actual = output(&array_chain(json!(42), depth));
        let expected = if depth < MAX_CONTAINER_DEPTH {
            wrap_array_schemas(json!([4]), depth)
        } else {
            wrap_array_schemas(json!([[], {"len": 1}]), MAX_CONTAINER_DEPTH - 1)
        };
        assert_eq!(actual, expected, "depth={depth}");
    }
}

#[test]
fn depth_limit_record_boundaries() {
    for depth in [17, 18, 19, 100] {
        let input = (0..depth).fold(json!(42), |value, _| json!({"child": value}));
        let (leaf, count) = if depth < MAX_CONTAINER_DEPTH {
            (json!([4]), depth)
        } else {
            (json!([{}]), MAX_CONTAINER_DEPTH - 1)
        };
        let expected = (0..count).fold(leaf, |value, _| json!([{"child": value}]));
        assert_eq!(output(&input), expected, "depth={depth}");
    }
}

#[test]
fn mixed_containers_share_one_depth_budget() {
    let input = (0..MAX_CONTAINER_DEPTH).fold(json!(42), |value, i| {
        if i % 2 == 0 {
            json!({"child": value})
        } else {
            json!([value])
        }
    });
    // Innermost record is at level 18 and is empty; all outer containers remain.
    let expected = (1..MAX_CONTAINER_DEPTH).fold(json!([{}]), |value, i| {
        if i % 2 == 0 {
            json!([{"child": value}])
        } else {
            json!([[value], {"len": 1}])
        }
    });
    assert_eq!(output(&input), expected);
}

#[test]
fn depth_exhaustion_still_reports_array_length_and_width_truncation() {
    let input = array_chain(json!(vec![0; 11]), MAX_CONTAINER_DEPTH - 1);
    let expected = wrap_array_schemas(
        json!([[], {"len": 11, "truncated": true}]),
        MAX_CONTAINER_DEPTH - 1,
    );
    assert_eq!(output(&input), expected);
}

#[test]
fn depth_exhaustion_still_reports_record_width_truncation() {
    let input = array_chain(wide_record(256), MAX_CONTAINER_DEPTH - 1);
    let expected = wrap_array_schemas(json!([{}, {"truncated": true}]), MAX_CONTAINER_DEPTH - 1);
    assert_eq!(output(&input), expected);
}

#[test]
fn empty_container_at_depth_limit_has_no_truncation_marker() {
    let input = array_chain(json!([]), MAX_CONTAINER_DEPTH - 1);
    let expected = wrap_array_schemas(json!([[], {"len": 0}]), MAX_CONTAINER_DEPTH - 1);
    assert_eq!(output(&input), expected);
}

#[test]
fn record_truncation_participates_in_array_deduplication() {
    let full = wide_record(MAX_RECORD_NODES);
    let mut truncated = full.clone();
    truncated
        .as_object_mut()
        .unwrap()
        .insert("zz-ignored".into(), json!(null));
    let input = json!([full, truncated]);
    let actual = output(&input);
    let variants = actual[0].as_array().unwrap();
    assert_eq!(variants.len(), 2);
    assert!(
        variants
            .iter()
            .any(|value| value.as_array().unwrap().len() == 1)
    );
    assert!(
        variants
            .iter()
            .any(|value| value.get(1) == Some(&json!({"truncated": true})))
    );
}

#[test]
fn same_types_under_different_keys_are_distinct() {
    let actual = output(&json!([{"a": 1}, {"b": 2}]));
    assert_eq!(actual[0].as_array().unwrap().len(), 2);
}

#[test]
fn associating_types_with_different_keys_is_distinct() {
    let actual = output(&json!([{"a": 1, "b": "x"}, {"a": "y", "b": 2}]));
    assert_eq!(actual[0].as_array().unwrap().len(), 2);
}

#[test]
fn canonical_output_is_identical_for_reordered_nested_inputs() {
    let left = r#"[{"b":[1,"x"],"a":true},{"different":null}]"#;
    let right = r#"[{"different":null},{"a":false,"b":["y",2]}]"#;
    assert_eq!(
        extract_schema_json(left).unwrap(),
        extract_schema_json(right).unwrap()
    );
}

#[test]
fn schema_equality_compares_canonical_structure() {
    let left = json!([{"b": [1, "x"], "a": true}, {"other": null}]);
    let right = json!([{"other": null}, {"a": false, "b": ["y", 2]}]);
    assert_eq!(extract_schema(&left), extract_schema(&right));
    assert_ne!(
        extract_schema(&left),
        extract_schema(&json!([{"other": null}]))
    );
}

#[test]
fn scalar_values_are_absent_from_serialization_and_debug_output() {
    let input = json!({
        "password": "private-secret-193847",
        "amount": 987_654_321,
        "nested": ["another-private-secret-2345"],
    });
    let schema = extract_schema(&input);
    let encoded = serde_json::to_string(&schema).unwrap();
    let debug = format!("{schema:?}");
    for secret in [
        "private-secret-193847",
        "987654321",
        "another-private-secret-2345",
    ] {
        assert!(!encoded.contains(secret));
        assert!(!debug.contains(secret));
    }
    assert_eq!(
        encoded,
        r#"[{"amount":[4],"nested":[[[8]],{"len":1}],"password":[8]}]"#
    );
}

#[test]
fn property_names_are_preserved_and_escaped() {
    let key = "quote\"slash\\\n\0é漢字😀";
    let input = Value::Object(core::iter::once((key.to_owned(), json!("not retained"))).collect());
    let encoded = extract_schema_json(&serde_json::to_string(&input).unwrap()).unwrap();
    let actual: Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(actual[0][key], json!([8]));
    assert!(!encoded.contains("not retained"));
}

#[test]
fn extraction_does_not_modify_input() {
    let input: Value = serde_json::from_str(r#"{"z":["b",2],"a":{"y":true,"x":1}}"#).unwrap();
    let before = serde_json::to_string(&input).unwrap();
    let schema = extract_schema(&input);
    assert_eq!(serde_json::to_string(&input).unwrap(), before);
    assert_eq!(
        serde_json::to_string(&schema).unwrap(),
        r#"[{"a":[{"x":[4],"y":[2]}],"z":[[[4],[8]],{"len":2}]}]"#
    );
}

#[test]
fn invalid_json_returns_a_structured_error() {
    for input in [
        "",
        "{",
        "[1,]",
        "{}{}",
        "NaN",
        "Infinity",
        "{\"key\":}",
        "null trailing",
        "\"\\uD800\"",
    ] {
        let error = extract_schema_json(input).unwrap_err();
        assert!(
            matches!(error, SchemaError::InvalidJson(_)),
            "input={input}"
        );
        assert!(core::error::Error::source(&error).is_some());
    }
}

#[test]
fn parse_error_preserves_location() {
    let error = extract_schema_json("{\n\"key\":\n}").unwrap_err();
    let SchemaError::InvalidJson(source) = error else {
        panic!("expected parse error");
    };
    assert_eq!(source.line(), 3);
    assert!(source.column() > 0);
}

#[test]
fn json_depth_truncation_does_not_require_a_deep_dom() {
    let input = format!("{}0{}", "[".repeat(256), "]".repeat(256));
    let actual: Value = serde_json::from_str(&extract_schema_json(&input).unwrap()).unwrap();
    let expected = wrap_array_schemas(json!([[], {"len": 1}]), MAX_CONTAINER_DEPTH - 1);
    assert_eq!(actual, expected);
}

#[test]
fn malformed_ignored_tail_is_still_rejected_by_json_parser() {
    assert!(matches!(
        extract_schema_json("[0,0,0,0,0,0,0,0,0,0,{"),
        Err(SchemaError::InvalidJson(_))
    ));
}

#[test]
fn duplicate_json_keys_keep_first_selected_value() {
    assert_eq!(
        extract_schema_json(r#"{"key":1,"key":"value"}"#).unwrap(),
        r#"[{"key":[4]}]"#
    );
}

#[test]
fn surrounding_json_whitespace_is_accepted() {
    assert_eq!(
        extract_schema_json(" \n\t [1,2] \r\n").unwrap(),
        r#"[[[4]],{"len":2}]"#
    );
}

#[test]
fn extraction_is_independent_between_threads() {
    let input = json!({"record": [{"id": 42, "name": "value"}, {"name": "other", "id": 23}]});
    let expected = serde_json::to_string(&extract_schema(&input)).unwrap();
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| serde_json::to_string(&extract_schema(&input)).unwrap()))
            .collect();
        for handle in handles {
            assert_eq!(handle.join().unwrap(), expected);
        }
    });
}

#[test]
fn serializer_propagates_writer_errors() {
    struct FailingWriter;
    impl std::io::Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("intentional writer failure"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let input = json!({"key": [1, true]});
    assert!(
        serde_json::to_writer(FailingWriter, &extract_schema(&input))
            .unwrap_err()
            .is_io()
    );
}

#[test]
fn duplicate_record_keys_count_toward_width_limit() {
    let input = format!("{{{}}}", vec![r#""child":[]"#; 265].join(","));
    assert_eq!(
        extract_schema_json(&input).unwrap(),
        r#"[{"child":[[],{"len":0}]},{"truncated":true}]"#
    );
}

#[test]
fn escaped_duplicate_keys_keep_first_value() {
    let input = r#"{"key":1,"\u006bey":"later"}"#;
    assert_eq!(extract_schema_json(input).unwrap(), r#"[{"key":[4]}]"#);
}

#[test]
fn json_parser_checks_discarded_record_values() {
    let fields: Vec<_> = (0..MAX_RECORD_NODES)
        .map(|i| format!(r#""key-{i}":0"#))
        .collect();
    let input = format!("{{{},\"tail\":[1,]}}", fields.join(","));
    assert!(matches!(
        extract_schema_json(&input),
        Err(SchemaError::InvalidJson(_))
    ));
}
