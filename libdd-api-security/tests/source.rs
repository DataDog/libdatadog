// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![allow(clippy::unwrap_used, clippy::expect_used)]

use libdd_api_security::{
    ScalarType, SchemaOptions, SchemaSource, SourceKind, extract_schema, extract_schema_from_source,
};
use serde_json::{Value, json};
extern crate alloc;
use alloc::borrow::Cow;
use core::cell::Cell;

struct Source<'a> {
    value: &'a Value,
    reads: &'a Cell<usize>,
}

impl<'a> Source<'a> {
    fn child(&self, value: &'a Value) -> Result<Self, &'static str> {
        self.reads.set(self.reads.get() + 1);
        if value == "poison" {
            return Err("visited poison");
        }
        Ok(Self {
            value,
            reads: self.reads,
        })
    }
}

impl<'a> SchemaSource<'a> for Source<'a> {
    type Error = &'static str;

    fn kind(&self) -> SourceKind {
        match self.value {
            Value::Null => SourceKind::Scalar(ScalarType::Null),
            Value::Bool(_) => SourceKind::Scalar(ScalarType::Boolean),
            Value::String(_) => SourceKind::Scalar(ScalarType::String),
            Value::Number(n) => SourceKind::Scalar(if n.is_i64() || n.is_u64() {
                ScalarType::Integer
            } else {
                ScalarType::Float
            }),
            Value::Array(values) => SourceKind::Array(values.len()),
            Value::Object(fields) => SourceKind::Record(fields.len()),
        }
    }

    fn element(&self, index: usize) -> Result<Self, Self::Error> {
        self.child(&self.value[index])
    }

    fn entry(&self, index: usize) -> Result<(Cow<'a, str>, Option<Self>), Self::Error> {
        let (key, value) = self.value.as_object().unwrap().iter().nth(index).unwrap();
        let child = if key.is_empty() || value == "omitted" {
            None
        } else {
            Some(self.child(value)?)
        };
        Ok((Cow::Borrowed(key), child))
    }
}

fn schema(value: &Value, reads: &Cell<usize>) -> Result<Value, &'static str> {
    extract_schema_from_source(&Source { value, reads }, SchemaOptions::default())
        .map(|schema| serde_json::to_value(schema).unwrap())
}

#[test]
fn matches_parsed_value_extraction_on_every_shared_fixture() {
    let fixtures: Value = serde_json::from_str(include_str!("fixtures/structural.json")).unwrap();
    for fixture in fixtures.as_array().unwrap() {
        let input: Value = serde_json::from_str(fixture["input"].as_str().unwrap()).unwrap();
        assert_eq!(
            schema(&input, &Cell::new(0)).unwrap(),
            serde_json::to_value(extract_schema(&input)).unwrap(),
            "{}",
            fixture["name"],
        );
    }
}

#[test]
fn never_reads_ignored_array_elements() {
    let mut input = vec![json!(true); 10];
    input.extend(vec![json!("poison"); 10_000]);
    let reads = Cell::new(0);
    assert_eq!(
        schema(&json!(input), &reads).unwrap(),
        json!([[[2]], {"len": 10010, "truncated": true}])
    );
    assert_eq!(reads.get(), 10);
}

#[test]
fn never_reads_ignored_record_values() {
    let mut fields = serde_json::Map::new();
    for index in 0..255 {
        fields.insert(format!("field-{index:03}"), json!(true));
    }
    fields.insert("last".into(), json!("poison"));
    let reads = Cell::new(0);
    let actual = schema(&Value::Object(fields), &reads).unwrap();
    assert_eq!(reads.get(), 255);
    assert_eq!(actual[0].as_object().unwrap().len(), 255);
    assert_eq!(actual[1], json!({"truncated":true}));
}

#[test]
fn empty_and_omitted_fields_consume_the_quota() {
    let mut fields = serde_json::Map::new();
    fields.insert(String::new(), json!("poison"));
    for index in 0..254 {
        fields.insert(format!("field-{index:03}"), json!("omitted"));
    }
    fields.insert("last".into(), json!("poison"));
    let reads = Cell::new(0);
    assert_eq!(
        schema(&Value::Object(fields), &reads).unwrap(),
        json!([{}, {"truncated":true}])
    );
    assert_eq!(reads.get(), 0);
}

#[test]
fn never_reads_children_at_depth_limit() {
    let mut input = json!("poison");
    for _ in 0..18 {
        input = json!([input]);
    }
    let reads = Cell::new(0);
    assert!(schema(&input, &reads).is_ok());
    assert_eq!(reads.get(), 17);
}

#[test]
fn preserves_adapter_errors_and_remains_usable() {
    assert_eq!(
        schema(&json!(["poison"]), &Cell::new(0)),
        Err("visited poison")
    );
    assert_eq!(
        schema(&json!({"key":"poison"}), &Cell::new(0)),
        Err("visited poison")
    );
    assert_eq!(schema(&json!(true), &Cell::new(0)).unwrap(), json!([2]));
}

#[test]
fn applies_representation_options_before_deduplication() {
    let input = json!([vec![1; 256], vec![1.5; 500]]);
    let reads = Cell::new(0);
    let schema = extract_schema_from_source(
        &Source {
            value: &input,
            reads: &reads,
        },
        SchemaOptions {
            numbers_as_float: true,
            array_length_limit: Some(256),
        },
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(schema).unwrap(),
        json!([[[[[16]], {"len":256,"truncated":true}]], {"len":2}])
    );
    assert_eq!(reads.get(), 22);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(512))]
    #[test]
    fn matches_existing_engine(
        values in proptest::collection::vec(proptest::prelude::any::<i64>(), 0..300),
        names in proptest::collection::vec("[a-z]{0,10}", 0..300),
    ) {
        let input = json!({"values":values, "names":names});
        proptest::prop_assert_eq!(schema(&input, &Cell::new(0)).unwrap(),
            serde_json::to_value(extract_schema(&input)).unwrap());
    }
}
