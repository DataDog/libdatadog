// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![allow(clippy::unwrap_used, clippy::expect_used)]

use libdd_api_security::{MAX_ARRAY_NODES, extract_schema, extract_schema_json};
use proptest::prelude::*;
use serde_json::{Value, json};

fn arbitrary_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|value| json!(value)),
        any::<u64>().prop_map(|value| json!(value)),
        any::<f64>()
            .prop_filter("finite JSON number", |value| value.is_finite())
            .prop_map(|value| json!(value)),
        ".{0,24}".prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 128, 8, |child| {
        prop_oneof![
            proptest::collection::vec(child.clone(), 0..9).prop_map(Value::Array),
            proptest::collection::btree_map(".{0,12}", child, 0..9)
                .prop_map(|fields| Value::Object(fields.into_iter().collect())),
        ]
    })
}

// Preserve the structure and numeric representation while changing values.
// Every generated container is below its width limit, so reversing it must not
// change which elements/fields are selected.
fn rearrange_and_replace(input: &Value) -> Value {
    match input {
        Value::Null => Value::Null,
        Value::Bool(value) => json!(!value),
        Value::String(_) => json!("replacement"),
        Value::Number(value) => {
            if value.is_i64() || value.is_u64() {
                json!(7)
            } else {
                json!(0.5)
            }
        }
        Value::Array(values) => {
            Value::Array(values.iter().rev().map(rearrange_and_replace).collect())
        }
        Value::Object(values) => Value::Object(
            values
                .iter()
                .rev()
                .map(|(key, value)| (key.clone(), rearrange_and_replace(value)))
                .collect(),
        ),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn equivalent_values_and_permutations_have_identical_schemas(input in arbitrary_json()) {
        let changed = rearrange_and_replace(&input);
        let original = extract_schema(&input);
        let equivalent = extract_schema(&changed);
        prop_assert_eq!(&original, &equivalent);
        prop_assert_eq!(
            serde_json::to_string(&original).unwrap(),
            serde_json::to_string(&equivalent).unwrap()
        );
    }

    #[test]
    fn parsed_and_json_apis_agree(input in arbitrary_json()) {
        let encoded = serde_json::to_string(&input).unwrap();
        prop_assert_eq!(
            extract_schema_json(&encoded).unwrap(),
            serde_json::to_string(&extract_schema(&input)).unwrap()
        );
    }

    #[test]
    fn repeated_structures_deduplicate_but_keep_original_length(
        value in arbitrary_json(), count in 1usize..80
    ) {
        let input = Value::Array(vec![value; count]);
        let result = serde_json::to_value(extract_schema(&input)).unwrap();
        prop_assert_eq!(result[0].as_array().unwrap().len(), 1);
        prop_assert_eq!(&result[1]["len"], &json!(count));
        prop_assert_eq!(result[1].get("truncated").is_some(), count > MAX_ARRAY_NODES);
    }

    #[test]
    fn modifying_discarded_array_tail_does_not_change_schema(
        prefix in proptest::collection::vec(arbitrary_json(), MAX_ARRAY_NODES),
        first in arbitrary_json(), second in arbitrary_json()
    ) {
        let mut left = prefix.clone();
        left.push(first);
        let mut right = prefix;
        right.push(second);
        let left = Value::Array(left);
        let right = Value::Array(right);
        prop_assert_eq!(extract_schema(&left), extract_schema(&right));
    }

    #[test]
    fn all_scalar_array_variants_are_types_only(values in proptest::collection::vec(
        prop_oneof![Just(Value::Null), any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(|value| json!(value)), ".{0,16}".prop_map(Value::String)],
        0..100
    )) {
        let input = Value::Array(values);
        let result = serde_json::to_value(extract_schema(&input)).unwrap();
        let variants = result[0].as_array().unwrap();
        prop_assert!(variants.len() <= 4);
        for variant in variants {
            prop_assert_eq!(variant.as_array().unwrap().len(), 1);
            prop_assert!([json!(1), json!(2), json!(4), json!(8)].contains(&variant[0]));
        }
    }
}
