// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![allow(clippy::unwrap_used, clippy::expect_used)]

use libdd_api_security::{SchemaOptions, extract_schema_json, extract_schema_json_with_options};
use serde_json::{Value, json};

const fn options() -> SchemaOptions {
    SchemaOptions {
        numbers_as_float: true,
        array_length_limit: Some(256),
    }
}

fn extract(input: &str) -> Value {
    serde_json::from_str(&extract_schema_json_with_options(input, options()).unwrap()).unwrap()
}

// Independent reference: normalize the existing public JSON schema after
// extraction, then deduplicate. This catches normalization ordering errors.
fn normalize(schema: &mut Value, convert_numbers: bool, limit: usize) {
    let entries = schema.as_array_mut().unwrap();
    match &mut entries[0] {
        Value::Number(value) if convert_numbers && *value == 4.into() => entries[0] = json!(16),
        Value::Object(fields) => {
            for value in fields.values_mut() {
                normalize(value, convert_numbers, limit);
            }
        }
        Value::Array(variants) => {
            for value in variants.iter_mut() {
                normalize(value, convert_numbers, limit);
            }
            variants.sort_by_cached_key(|value| serde_json::to_string(value).unwrap());
            variants.dedup();
            let len = entries[1]["len"].as_u64().unwrap();
            entries[1]["len"] = json!(len.min(u64::try_from(limit).unwrap()));
        }
        _ => {}
    }
}

#[test]
fn normalizes_numbers_before_nested_variant_deduplication() {
    assert_eq!(
        extract("[1,1.5,-2,18446744073709551615]"),
        json!([[[16]], {"len":4}])
    );
    assert_eq!(
        extract(r#"[{"id":1},{"id":1.5}]"#),
        json!([[[{"id":[16]}]], {"len":2}])
    );
    assert_eq!(
        extract("[[1],[1.5]]"),
        json!([[[[[16]], {"len":1}]], {"len":2}])
    );
}

#[test]
fn normalizes_lengths_before_parent_deduplication() {
    let first = vec![true; 256];
    let second = vec![true; 500];
    let input = serde_json::to_string(&json!([first, second])).unwrap();
    assert_eq!(
        extract(&input),
        json!([[[[[2]], {"len":256,"truncated":true}]], {"len":2}])
    );
}

#[test]
fn length_limit_is_metadata_only_and_preserves_truncation() {
    for len in [0, 1, 9, 10, 11, 255, 256, 257, 1000] {
        let input = serde_json::to_string(&vec![true; len]).unwrap();
        let result = extract(&input);
        assert_eq!(result[1]["len"], json!(len.min(256)));
        assert_eq!(result[1].get("truncated").is_some(), len > 10);
    }
    let value = extract_schema_json_with_options(
        "[true,false]",
        SchemaOptions {
            array_length_limit: Some(0),
            ..SchemaOptions::default()
        },
    )
    .unwrap();
    assert_eq!(value, r#"[[[2]],{"len":0}]"#);
}

#[test]
fn normalizes_all_shared_fixtures_like_the_reference() {
    let fixtures: Value = serde_json::from_str(include_str!("fixtures/structural.json")).unwrap();
    for fixture in fixtures.as_array().unwrap() {
        let input = fixture["input"].as_str().unwrap();
        let mut expected: Value =
            serde_json::from_str(&extract_schema_json(input).unwrap()).unwrap();
        normalize(&mut expected, true, 256);
        let mut actual = extract(input);
        normalize(&mut actual, false, usize::MAX);
        assert_eq!(actual, expected, "{}", fixture["name"]);
        assert_eq!(
            extract_schema_json_with_options(input, SchemaOptions::default()).unwrap(),
            extract_schema_json(input).unwrap()
        );
    }
}

#[test]
fn options_do_not_disable_json_validation_in_discarded_tails() {
    for input in ["[1,2,3,4,5,6,7,8,9,10,]", r#"{"id":1}{}"#, "1e400"] {
        assert!(extract_schema_json_with_options(input, options()).is_err());
    }
}

#[test]
fn normalized_length_preserves_deep_array_metadata() {
    let input =
        "[".repeat(18) + &serde_json::to_string(&vec![true; 500]).unwrap() + &"]".repeat(18);
    let mut result = extract(&input);
    for _ in 1..18 {
        result = result[0][0].clone();
    }
    assert_eq!(result, json!([[], {"len":1}]));
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(512))]
    #[test]
    fn normalizes_mixed_nested_arrays_like_the_reference(
        integers in proptest::collection::vec(proptest::prelude::any::<i64>(), 0..30),
        decimals in proptest::collection::vec(-1e10f64..1e10, 0..30),
        lengths in proptest::collection::vec(0usize..520, 0..12)
    ) {
        let input = serde_json::to_string(&json!({
            "values": [integers, decimals],
            "arrays": lengths.into_iter().map(|len| vec![1;len]).collect::<Vec<_>>()
        })).unwrap();
        let mut expected: Value = serde_json::from_str(&extract_schema_json(&input).unwrap()).unwrap();
        normalize(&mut expected, true, 256);
        let mut actual = extract(&input);
        normalize(&mut actual, false, usize::MAX);
        proptest::prop_assert_eq!(actual, expected);
    }
}
