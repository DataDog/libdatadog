// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable,
    clippy::panic
)]

use libdd_api_security::{
    Matcher, ScalarType, Scanner, ScannerSource, SchemaOptions, SchemaSource, SourceKind,
    extract_schema_from_source_with_scanners, extract_schema_json_with_scanners,
    extract_schema_with_scanners,
};
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
extern crate alloc;
use alloc::{borrow::Cow, collections::BTreeMap, fmt};
use core::convert::Infallible;

// Test-only input tree retains duplicate keys and non-JSON invalid values,
// both of which serde_json::Value cannot represent.
enum Input {
    Scalar(Value),
    Array(Vec<Self>),
    Record(Vec<(String, Self)>),
    Unknown,
}

impl<'de> Deserialize<'de> for Input {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct InputVisitor;
        impl<'de> Visitor<'de> for InputVisitor {
            type Value = Input;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a value")
            }
            fn visit_unit<E>(self) -> Result<Input, E> {
                Ok(Input::Scalar(Value::Null))
            }
            fn visit_bool<E>(self, v: bool) -> Result<Input, E> {
                Ok(Input::Scalar(json!(v)))
            }
            fn visit_i64<E>(self, v: i64) -> Result<Input, E> {
                Ok(Input::Scalar(json!(v)))
            }
            fn visit_u64<E>(self, v: u64) -> Result<Input, E> {
                Ok(Input::Scalar(json!(v)))
            }
            fn visit_f64<E>(self, v: f64) -> Result<Input, E> {
                Ok(Input::Scalar(json!(v)))
            }
            fn visit_str<E>(self, v: &str) -> Result<Input, E> {
                Ok(Input::Scalar(json!(v)))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Input, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = a.next_element()? {
                    values.push(value);
                }
                Ok(Input::Array(values))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Input, A::Error> {
                let mut fields = Vec::new();
                while let Some(entry) = a.next_entry()? {
                    fields.push(entry);
                }
                Ok(Input::Record(fields))
            }
        }
        d.deserialize_any(InputVisitor)
    }
}

struct Source<'a>(&'a Input);
impl<'a> SchemaSource<'a> for Source<'a> {
    type Error = Infallible;
    fn kind(&self) -> SourceKind {
        match self.0 {
            Input::Unknown => SourceKind::Scalar(ScalarType::Unknown),
            Input::Array(values) => SourceKind::Array(values.len()),
            Input::Record(fields) => SourceKind::Record(fields.len()),
            Input::Scalar(value) => SourceKind::Scalar(match value {
                Value::Null => ScalarType::Null,
                Value::Bool(_) => ScalarType::Boolean,
                Value::String(_) => ScalarType::String,
                Value::Number(n) if n.is_i64() || n.is_u64() => ScalarType::Integer,
                Value::Number(_) => ScalarType::Float,
                _ => unreachable!("test parser separates containers"),
            }),
        }
    }
    fn element(&self, index: usize) -> Result<Self, Self::Error> {
        let Input::Array(values) = self.0 else {
            unreachable!()
        };
        Ok(Self(&values[index]))
    }
    fn entry(&self, index: usize) -> Result<(Cow<'a, str>, Option<Self>), Self::Error> {
        let Input::Record(fields) = self.0 else {
            unreachable!()
        };
        let (key, value) = &fields[index];
        Ok((Cow::Borrowed(key), Some(Self(value))))
    }
}
impl<'a> ScannerSource<'a> for Source<'a> {
    fn string(&self) -> Result<Cow<'a, str>, Self::Error> {
        let Input::Scalar(Value::String(value)) = self.0 else {
            unreachable!()
        };
        Ok(Cow::Borrowed(value))
    }
}

fn mark_unknown(input: &mut Input, path: &[Value]) {
    let Some((head, tail)) = path.split_first() else {
        *input = Input::Unknown;
        return;
    };
    match input {
        Input::Array(values) => mark_unknown(
            &mut values[usize::try_from(head.as_u64().unwrap()).unwrap()],
            tail,
        ),
        Input::Record(fields) => {
            let (_, value) = fields
                .iter_mut()
                .find(|(key, _)| key == head.as_str().unwrap())
                .unwrap();
            mark_unknown(value, tail);
        }
        _ => panic!("invalid reference path"),
    }
}

fn canonical(schema: &mut Value) {
    match &mut schema.as_array_mut().unwrap()[0] {
        Value::Array(variants) => {
            for child in variants.iter_mut() {
                canonical(child);
            }
            variants.sort_by_cached_key(|v| serde_json::to_string(v).unwrap());
        }
        Value::Object(fields) => {
            for child in fields.values_mut() {
                canonical(child);
            }
            fields.sort_keys();
        }
        _ => {}
    }
}

fn assert_schema(mut actual: Value, mut expected: Value) {
    canonical(&mut actual);
    canonical(&mut expected);
    assert_eq!(actual, expected);
}

fn check_reference(name: &str) {
    let cases: Value = serde_json::from_str(include_str!("fixtures/libddwaf-schema.json")).unwrap();
    let case = cases
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap();
    let scanners: Vec<_> = case["scanners"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let matcher = |v: &Value| {
                v.as_str()
                    .map(|pattern| Matcher::regex(pattern, pattern.len(), true).unwrap())
            };
            Scanner::new(
                s["id"].as_str().unwrap().into(),
                serde_json::from_value(s["tags"].clone()).unwrap(),
                matcher(&s["key"]),
                matcher(&s["value"]),
            )
            .unwrap()
        })
        .collect();
    for encoded in case["inputs"].as_array().unwrap() {
        let text = encoded.as_str().unwrap();
        let mut input: Input = serde_json::from_str(text).unwrap();
        if let Some(paths) = case["unknown_paths"].as_array() {
            for path in paths {
                mark_unknown(&mut input, path.as_array().unwrap());
            }
        }
        let schema = extract_schema_from_source_with_scanners(
            &Source(&input),
            SchemaOptions::default(),
            &scanners,
        )
        .unwrap();
        assert_schema(
            serde_json::to_value(schema).unwrap(),
            case["schema"].clone(),
        );
        if case.get("unknown_paths").is_none() {
            let schema =
                extract_schema_json_with_scanners(text, SchemaOptions::default(), &scanners)
                    .unwrap();
            assert_schema(
                serde_json::from_str(&schema).unwrap(),
                case["schema"].clone(),
            );
        }
        if case["parsed_value_representable"] == true {
            let parsed: Value = serde_json::from_str(text).unwrap();
            assert_schema(
                serde_json::to_value(extract_schema_with_scanners(&parsed, &scanners)).unwrap(),
                case["schema"].clone(),
            );
        }
    }
}

mod extract_schema {
    use super::check_reference;
    macro_rules! reference_tests {
        ($($test:ident => $name:literal),* $(,)?) => {$(
            #[test] fn $test() { check_reference($name); }
        )*};
    }
    reference_tests!(
        unknown_scalar_schema => "UnknownScalarSchema",
        null_scalar_schema => "NullScalarSchema",
        bool_scalar_schema => "BoolScalarSchema",
        int_scalar_schema => "IntScalarSchema",
        string_scalar_schema => "StringScalarSchema",
        float_scalar_schema => "FloatScalarSchema",
        empty_array_schema => "EmptyArraySchema",
        array_schema => "ArraySchema",
        array_with_duplicate_scalar_schema => "ArrayWithDuplicateScalarSchema",
        array_with_duplicate_maps_schema => "ArrayWithDuplicateMapsSchema",
        array_with_duplicate_arrays_schema => "ArrayWithDuplicateArraysSchema",
        array_with_duplicate_containers_schema => "ArrayWithDuplicateContainersSchema",
        empty_map_schema => "EmptyMapSchema",
        map_schema => "MapSchema",
        depth_limit => "DepthLimit",
        array_nodes_limit => "ArrayNodesLimit",
        record_nodes_limit => "RecordNodesLimit",
        schema_with_single_scanner => "SchemaWithSingleScanner",
        schema_with_multiple_scanners => "SchemaWithMultipleScanners",
        schema_with_scanner_no_match => "SchemaWithScannerNoMatch",
        schema_with_scanner_single_value_no_key => "SchemaWithScannerSingleValueNoKey",
        schema_with_scanner_array_no_key => "SchemaWithScannerArrayNoKey",
        schema_with_scanner_array_with_key => "SchemaWithScannerArrayWithKey",
        schema_with_scanner_nested_array_with_key => "SchemaWithScannerNestedArrayWithKey",
    );
}

mod scanner {
    use super::*;

    fn configured_named(id: &str, tags: &[(&str, &str)], key: bool, value: bool) -> Scanner {
        let tags: BTreeMap<String, String> = tags
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect();
        let scanner = Scanner::new(
            id.into(),
            tags.clone(),
            key.then(|| Matcher::exact(vec!["hello".into(), "goodbye".into()])),
            value.then(|| Matcher::ip(&["192.168.0.1"]).unwrap()),
        )
        .unwrap();
        assert_eq!(scanner.id(), id);
        assert_eq!(scanner.tags(), &tags);
        scanner
    }

    fn configured(key: bool, value: bool) -> Scanner {
        configured_named(
            "something",
            &[("type", "PII"), ("category", "IP")],
            key,
            value,
        )
    }
    #[test]
    fn simple_match() {
        let scanner = configured(true, true);
        assert_eq!(scanner.id(), "something");
        assert_eq!(
            serde_json::to_value(scanner.tags()).unwrap(),
            json!({"type":"PII","category":"IP"})
        );
        assert!(scanner.matches("hello", Some("192.168.0.1")));
    }
    #[test]
    fn simple_match_no_key_matcher() {
        assert!(configured(false, true).matches("hello", Some("192.168.0.1")));
    }
    #[test]
    fn simple_match_no_value_matcher() {
        assert!(configured(true, false).matches("hello", Some("192.168.0.1")));
    }
    #[test]
    fn no_match_on_key() {
        assert!(
            !configured_named(
                "0",
                &[("type", "PII"), ("category", "IP"), ("danger", "0")],
                true,
                true
            )
            .matches("helloo", Some("192.168.0.1"))
        );
    }
    #[test]
    fn no_match_on_value() {
        assert!(!configured_named("null", &[], true, true).matches("hello", Some("192.168.0.2")));
    }
    #[test]
    fn invalid_key() {
        assert!(
            !configured_named(
                "0",
                &[("type", "PII"), ("category", "IP"), ("danger", "0")],
                true,
                true
            )
            .matches("", Some("192.168.0.1"))
        );
    }
    #[test]
    fn invalid_value() {
        assert!(!configured_named("null", &[], true, true).matches("hello", None));
    }

    #[test]
    fn metadata_prevents_incorrect_variant_deduplication() {
        let scanners: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|value| {
                Scanner::new(
                    value.into(),
                    [("category".into(), value.into())].into(),
                    None,
                    Some(Matcher::exact(vec![value.into()])),
                )
                .unwrap()
            })
            .collect();
        let input = json!(["first", "second", "unclassified", "first"]);
        let schema = serde_json::to_value(extract_schema_with_scanners(&input, &scanners)).unwrap();
        assert_schema(
            schema,
            json!([[[8,{"category":"first"}],[8,{"category":"second"}],[8]],{"len":4}]),
        );
    }
    #[test]
    fn empty_tags_still_stop_at_first_matching_scanner() {
        let scanners = [
            Scanner::new(
                "first".into(),
                BTreeMap::default(),
                None,
                Some(Matcher::regex(".*", 0, true).unwrap()),
            )
            .unwrap(),
            Scanner::new(
                "second".into(),
                [("type".into(), "PII".into())].into(),
                None,
                Some(Matcher::regex(".*", 0, true).unwrap()),
            )
            .unwrap(),
        ];
        let schema =
            extract_schema_json_with_scanners(r#""data""#, SchemaOptions::default(), &scanners)
                .unwrap();
        assert_eq!(schema, "[8]");
    }
    #[test]
    fn regex_options_and_invalid_configuration() {
        let scanner = Scanner::new(
            "case".into(),
            BTreeMap::default(),
            None,
            Some(Matcher::regex("email", 5, false).unwrap()),
        )
        .unwrap();
        assert!(scanner.matches("", Some("EMAIL")));
        assert!(!scanner.matches("", Some("mail")));
        let empty = Scanner::new(
            "empty".into(),
            BTreeMap::default(),
            None,
            Some(Matcher::regex(".*", 0, true).unwrap()),
        )
        .unwrap();
        assert!(!empty.matches("", Some("")));
        assert!(Matcher::regex("[", 0, true).is_err());
        assert!(Matcher::ip(&["not an IP"]).is_err());
        assert!(Scanner::new("none".into(), BTreeMap::default(), None, None).is_err());
    }
    #[test]
    fn ip_networks_and_missing_optional_conditions() {
        let scanner = Scanner::new(
            "network".into(),
            BTreeMap::default(),
            None,
            Some(Matcher::ip(&["192.168.0.0/24", "2001:db8::/32"]).unwrap()),
        )
        .unwrap();
        assert!(scanner.matches("", Some("192.168.0.255")));
        assert!(scanner.matches("", Some("2001:db8::1")));
        assert!(!scanner.matches("", Some("192.168.1.1")));
        assert!(!scanner.matches("", None));
        assert!(configured(true, false).matches("hello", None));
    }
    #[test]
    fn classification_does_not_replenish_array_quota() {
        let scanners = [Scanner::new(
            "secret".into(),
            [("type".into(), "secret".into())].into(),
            None,
            Some(Matcher::exact(vec!["secret".into()])),
        )
        .unwrap()];
        let mut values = vec!["ordinary"; 10];
        values.push("secret");
        let encoded = serde_json::to_string(&values).unwrap();
        let schema =
            extract_schema_json_with_scanners(&encoded, SchemaOptions::default(), &scanners)
                .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&schema).unwrap(),
            json!([[[8]],{"len":11,"truncated":true}])
        );
    }
}

#[test]
fn native_email_scanner_pattern_and_ascii_shorthands() {
    let config: Value =
        serde_json::from_str(include_str!("fixtures/libddwaf-scanners.json")).unwrap();
    let definition = &config["scanners"][0];
    let condition = |name: &str| {
        let params = &definition[name]["parameters"];
        Matcher::regex(params["regex"].as_str().unwrap(), 5, false).unwrap()
    };
    let scanner = Scanner::new(
        "scanner-001".into(),
        serde_json::from_value(definition["tags"].clone()).unwrap(),
        Some(condition("key")),
        Some(condition("value")),
    )
    .unwrap();
    assert!(scanner.matches("email", Some("data@datadoghq.com")));
    assert!(!scanner.matches("email", Some("invalid")));
    assert!(!scanner.matches("mail", Some("data@datadoghq.com")));

    let word = Scanner::new(
        "ascii".into(),
        BTreeMap::default(),
        None,
        Some(Matcher::regex(r"^\w+$", 0, true).unwrap()),
    )
    .unwrap();
    assert!(word.matches("", Some("abc_123")));
    assert!(!word.matches("", Some("café")));
    let digit = Scanner::new(
        "digit".into(),
        BTreeMap::default(),
        None,
        Some(Matcher::regex(r"^\d+$", 0, true).unwrap()),
    )
    .unwrap();
    assert!(digit.matches("", Some("123")));
    assert!(!digit.matches("", Some("١٢٣")));
}

#[test]
fn regex_compatibility_preserves_literals_and_unicode_properties() {
    for (pattern, value, expected) in [
        (r"^\W+$", "é", true),
        (r"^\W+$", "abc", false),
        (r"^\D+$", "١٢٣", true),
        (r"^\D+$", "123", false),
        (r"^\s$", "\u{000b}", false),
        (r"^\S$", "\u{000b}", true),
        (r"^\s$", "\u{00a0}", false),
        (r"^\s$", "\t", true),
        (r"\ba\b", "éaé", true),
        (r"\Ba\B", "bab", true),
        (r"^[\w]+$", "abc_123", true),
        (r"^[^\W]+$", "é", false),
        (r"^\\w$", r"\w", true),
        (r"^\p{L}+$", "café", true),
        (r"^.$", "é", true),
    ] {
        let scanner = Scanner::new(
            "regex".into(),
            BTreeMap::default(),
            None,
            Some(Matcher::regex(pattern, 0, true).unwrap()),
        )
        .unwrap();
        assert_eq!(
            scanner.matches("", Some(value)),
            expected,
            "{pattern:?} {value:?}"
        );
    }
}

mod configuration {
    use super::*;
    use libdd_api_security::load_scanners;

    fn check_reference(name: &str) {
        let cases: Value =
            serde_json::from_str(include_str!("fixtures/libddwaf-scanner-configuration.json"))
                .unwrap();
        let case = cases
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == name)
            .unwrap();
        let result = load_scanners(&case["definitions"], &semver::Version::new(2, 1, 0)).unwrap();
        assert_eq!(
            serde_json::to_value(&result.diagnostics).unwrap(),
            case["diagnostics"]
        );
        assert_eq!(
            result.scanners.keys().collect::<Vec<_>>(),
            result.diagnostics.loaded.iter().collect::<Vec<_>>()
        );
        if let Some(scanner) = result.scanners.get("ecd") {
            assert_eq!(scanner.id(), "ecd");
            assert_eq!(
                serde_json::to_value(scanner.tags()).unwrap(),
                json!({"type":"email","category":"pii"})
            );
            for check in case["matches"].as_array().unwrap() {
                assert_eq!(
                    scanner.matches(check["key"].as_str().unwrap(), check["value"].as_str()),
                    check["match"].as_bool().unwrap()
                );
            }
        }
    }

    macro_rules! reference_tests {
        ($($test:ident => $name:literal),* $(,)?) => {$(
            #[test] fn $test() { check_reference($name); }
        )*};
    }
    reference_tests!(
        parse_key_only_scanner => "ParseKeyOnlyScanner",
        parse_value_only_scanner => "ParseValueOnlyScanner",
        parse_key_value_scanner => "ParseKeyValueScanner",
        parse_no_i_d => "ParseNoID",
        parse_no_tags => "ParseNoTags",
        parse_no_key_value => "ParseNoKeyValue",
        parse_duplicate => "ParseDuplicate",
        parse_key_no_operator => "ParseKeyNoOperator",
        parse_key_no_parameters => "ParseKeyNoParameters",
        parse_value_no_operator => "ParseValueNoOperator",
        parse_value_no_parameters => "ParseValueNoParameters",
        parse_unknown_matcher => "ParseUnknownMatcher",
        parse_rule_data_i_d => "ParseRuleDataID",
        incompatible_min_version => "IncompatibleMinVersion",
        incompatible_max_version => "IncompatibleMaxVersion",
        compatible_version => "CompatibleVersion",
    );

    #[test]
    fn valid_neighbors_survive_malformed_scanners() {
        let definitions = json!([
            {"id":"good","tags":{},"value":{"operator":"exact_match","parameters":{"list":["value"]}}},
            {"id":"bad","tags":{}},
            null,
            {"id":"network","tags":{},"value":{"operator":"ip_match","parameters":{"list":["192.168.0.0/24"]}}}
        ]);
        let result = load_scanners(&definitions, &semver::Version::new(2, 1, 0)).unwrap();
        assert_eq!(result.scanners.len(), 2);
        assert_eq!(result.diagnostics.failed.len(), 2);
        assert!(result.diagnostics.failed.contains("bad"));
        assert!(result.diagnostics.failed.contains("index:2"));
        assert!(result.scanners["good"].matches("", Some("value")));
        assert!(result.scanners["network"].matches("", Some("192.168.0.1")));
        assert!(load_scanners(&json!({}), &semver::Version::new(2, 1, 0)).is_err());
    }

    #[test]
    fn loads_real_reference_email_configuration() {
        let document: Value =
            serde_json::from_str(include_str!("fixtures/libddwaf-scanners.json")).unwrap();
        let result = load_scanners(&document["scanners"], &semver::Version::new(2, 1, 0)).unwrap();
        assert!(result.diagnostics.errors.is_empty());
        assert_eq!(result.scanners.len(), 1);
        let scanner = &result.scanners["scanner-001"];
        assert!(scanner.matches("email", Some("data@datadoghq.com")));
        assert!(!scanner.matches("email", Some("invalid")));
        assert!(!scanner.matches("mail", Some("data@datadoghq.com")));
    }
}

mod processors {
    use super::*;
    use libdd_api_security::load_processors;
    fn check_reference(name: &str) {
        let cases: Value = serde_json::from_str(include_str!(
            "fixtures/libddwaf-processors-configuration.json"
        ))
        .unwrap();
        let case = cases
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap();
        let result = load_processors(&case["definitions"], &semver::Version::new(2, 1, 0)).unwrap();
        assert_eq!(
            serde_json::to_value(&result.diagnostics).unwrap(),
            case["diagnostics"]
        );

        assert_eq!(
            result.processors.keys().collect::<Vec<_>>(),
            result.diagnostics.loaded.iter().collect::<Vec<_>>()
        );
        if let Some(processor) = result.processors.get("1") {
            let first = &case["definitions"][0];
            assert_eq!(
                processor.evaluate,
                first["evaluate"].as_bool().unwrap_or(true)
            );
            assert_eq!(processor.output, first["output"].as_bool().unwrap_or(false));
            assert!(processor.conditions.is_empty());
            assert!(processor.scanners.include.is_empty());
            assert_eq!(processor.mappings.len(), 1);
            assert_eq!(processor.mappings[0].input.address, "in");
            assert!(processor.mappings[0].input.key_path.is_empty());
            assert_eq!(processor.mappings[0].output, "out");
        }
    }
    macro_rules! reference_tests {
        ($($test:ident => $name:literal),* $(,)?) => {$(
            #[test] fn $test() { check_reference($name); }
        )*};
    }
    reference_tests!(
        parse_no_generator => "ParseNoGenerator",
        parse_no_i_d => "ParseNoID",
        parse_no_parameters => "ParseNoParameters",
        parse_no_mappings => "ParseNoMappings",
        parse_empty_mappings => "ParseEmptyMappings",
        parse_no_input => "ParseNoInput",
        parse_empty_input => "ParseEmptyInput",
        parse_no_output => "ParseNoOutput",
        parse_unknown_generator => "ParseUnknownGenerator",
        parse_useless => "ParseUseless",
        parse_preprocessor => "ParsePreprocessor",
        parse_preprocessor_with_output => "ParsePreprocessorWithOutput",
        parse_postprocessor => "ParsePostprocessor",
        parse_duplicate => "ParseDuplicate",
        incompatible_min_version => "IncompatibleMinVersion",
        incompatible_max_version => "IncompatibleMaxVersion",
        compatible_version => "CompatibleVersion",
    );
}

mod overrides {
    use super::*;
    use libdd_api_security::load_processor_overrides;
    fn check_reference(name: &str) {
        let cases: Value = serde_json::from_str(include_str!(
            "fixtures/libddwaf-overrides-configuration.json"
        ))
        .unwrap();
        let case = cases
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap();
        let result = load_processor_overrides(&case["definitions"]).unwrap();
        assert_eq!(
            serde_json::to_value(&result.diagnostics).unwrap(),
            case["diagnostics"]
        );

        assert_eq!(result.overrides.len(), result.diagnostics.loaded.len());
        let parse_ref = |value: &Value| {
            if let Some(id) = value["id"].as_str() {
                libdd_api_security::ScannerReference::Id(id.into())
            } else {
                libdd_api_security::ScannerReference::Tags(
                    serde_json::from_value(value["tags"].clone()).unwrap(),
                )
            }
        };
        for (loaded, definition) in result
            .overrides
            .iter()
            .zip(case["definitions"].as_array().unwrap())
        {
            assert_eq!(
                loaded.targets,
                definition["target"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(parse_ref)
                    .collect::<Vec<_>>()
            );
            for (actual, key) in [
                (&loaded.scanners.include, "include"),
                (&loaded.scanners.exclude, "exclude"),
            ] {
                let expected = definition["scanners"][key]
                    .as_array()
                    .map(|v| v.iter().map(parse_ref).collect::<Vec<_>>())
                    .unwrap_or_default();
                assert_eq!(*actual, expected);
            }
        }
    }
    macro_rules! reference_tests {
        ($($test:ident => $name:literal),* $(,)?) => {$(
            #[test] fn $test() { check_reference($name); }
        )*};
    }
    reference_tests!(
        parse_override_without_targets => "ParseOverrideWithoutTargets",
        parse_override_with_target_by_tags => "ParseOverrideWithTargetByTags",
        parse_override_without_scanners => "ParseOverrideWithoutScanners",
        parse_override_with_scanner_by_id => "ParseOverrideWithScannerById",
        parse_override_with_excluded_scanner_by_id => "ParseOverrideWithExcludedScannerById",
        parse_override_with_scanner_by_tags => "ParseOverrideWithScannerByTags",
        parse_override_with_excluded_scanner_by_tags => "ParseOverrideWithExcludedScannerByTags",
        parse_override_with_multiple_targets => "ParseOverrideWithMultipleTargets",
        parse_override_with_include_and_exclude_scanners => "ParseOverrideWithIncludeAndExcludeScanners",
        parse_override_with_multiple_targets_and_scanners => "ParseOverrideWithMultipleTargetsAndScanners",
    );
}

// Ports the schema-specific assertions in the native integration suites.
// WAF rule matches are replaced by assertions on the schemas routed to
// evaluation; this library deliberately has no attack/event engine.
fn run_collection_reference(cases: &Value, name: &str) {
    use libdd_api_security::CollectionBuilder;
    let case = cases["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap();
    let mut builder = CollectionBuilder::default();
    let mut collector = builder.build();
    let mut persistent = serde_json::Map::new();
    for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
        match step["op"].as_str().unwrap() {
            "reset" => {
                builder = CollectionBuilder::default();
                persistent.clear();
            }
            "update" => {
                let diagnostics = builder
                    .update(
                        step["source"].as_str().unwrap().into(),
                        &cases["configurations"][step["configuration"].as_str().unwrap()],
                        &semver::Version::new(2, 1, 0),
                    )
                    .unwrap();
                for section in diagnostics.sections.values() {
                    let libdd_api_security::SectionDiagnostics::Items(items) = section else {
                        panic!("invalid reference section")
                    };
                    assert!(items.failed.is_empty(), "{name} update {index}: {items:?}");
                }
            }
            "remove" => {
                assert!(builder.remove(step["source"].as_str().unwrap()));
            }
            "build" => {
                collector = builder.build();
            }
            "collect" => {
                let input = step["input"].as_object().unwrap();
                let context = step["context"].as_str().unwrap();
                if context == "persistent" {
                    persistent.extend(input.clone());
                }
                let generated = collector.collect(|address| {
                    if context == "map" {
                        input.get(address)
                    } else {
                        input.get(address).or_else(|| persistent.get(address))
                    }
                });
                let mut attributes = serde_json::Map::new();
                let mut evaluation = serde_json::Map::new();
                for item in generated {
                    let schema = serde_json::to_value(item.schema).unwrap();
                    if item.evaluate {
                        evaluation.insert(item.address.into(), schema.clone());
                    }
                    if item.output {
                        attributes.insert(item.address.into(), schema);
                    }
                }
                assert_eq!(
                    Value::Object(attributes),
                    step["attributes"],
                    "{name} step {index}"
                );
                for (address, schema) in step["evaluation"].as_object().unwrap() {
                    assert_eq!(
                        evaluation.get(address),
                        Some(schema),
                        "{name} evaluation {index}"
                    );
                }
            }
            other => panic!("unknown reference operation {other}"),
        }
    }
}

mod integration_extract_schema {
    use super::*;
    fn check_reference(name: &str) {
        let cases: Value = serde_json::from_str(include_str!(
            "fixtures/libddwaf-integration-extract_schema.json"
        ))
        .unwrap();
        run_collection_reference(&cases, name);
    }
    macro_rules! reference_tests {
        ($($test:ident => $name:literal),* $(,)?) => {$(
            #[test] fn $test() { check_reference($name); }
        )*};
    }
    reference_tests!(
        postprocessor => "Postprocessor",
        preprocessor => "Preprocessor",
        processor => "Processor",
        processor_with_scanner_by_tags => "ProcessorWithScannerByTags",
        processor_with_scanner_by_i_d => "ProcessorWithScannerByID",
        processor_update => "ProcessorUpdate",
        scanner_update => "ScannerUpdate",
        processor_and_scanner_update => "ProcessorAndScannerUpdate",
        empty_scanner_update => "EmptyScannerUpdate",
        empty_processor_update => "EmptyProcessorUpdate",
        postprocessor_with_subcontext_mapping => "PostprocessorWithSubcontextMapping",
        preprocessor_with_subcontext_mapping => "PreprocessorWithSubcontextMapping",
        processor_subcontext_expression => "ProcessorSubcontextExpression",
    );
}

mod integration_overrides {
    use super::*;
    fn check_reference(name: &str) {
        let cases: Value =
            serde_json::from_str(include_str!("fixtures/libddwaf-integration-overrides.json"))
                .unwrap();
        run_collection_reference(&cases, name);
    }
    macro_rules! reference_tests {
        ($($test:ident => $name:literal),* $(,)?) => {$(
            #[test] fn $test() { check_reference($name); }
        )*};
    }
    reference_tests!(
        add_scanners_by_id => "AddScannersById",
        add_scanners_by_tags => "AddScannersByTags",
        add_scanner_to_populated_processor => "AddScannerToPopulatedProcessor",
        disable_default_scanners => "DisableDefaultScanners",
        remove_scanners_after_override => "RemoveScannersAfterOverride",
        remove_override => "RemoveOverride",
        override_multiple_processors => "OverrideMultipleProcessors",
        scanners_precedence_id_vs_tags_on_empty_processor => "ScannersPrecedenceIdVsTagsOnEmptyProcessor",
        scanners_precedence_on_default_processor => "ScannersPrecedenceOnDefaultProcessor",
        include_multiple_scanners_by_id => "IncludeMultipleScannersById",
        include_multiple_scanners_by_tags => "IncludeMultipleScannersByTags",
        include_then_exclude_multiple_scanners_by_id_on_headers => "IncludeThenExcludeMultipleScannersByIdOnHeaders",
        include_multiple_by_tags_exclude_one_by_id => "IncludeMultipleByTagsExcludeOneById",
    );
}

mod diagnostics {
    use super::*;
    use libdd_api_security::CollectionBuilder;

    #[test]
    fn processor() {
        let input: Value =
            serde_json::from_str(include_str!("fixtures/libddwaf-diagnostics-processor.json"))
                .unwrap();
        let actual = CollectionBuilder::default()
            .update("rules".into(), &input, &semver::Version::new(2, 1, 0))
            .unwrap();
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            json!({
                "ruleset_version":"1.8.0",
                "processors":{"loaded":["processor-001"],"failed":[],"skipped":[],"errors":{},"warnings":{}}
            })
        );
    }

    fn invalid_container(section: &str) {
        let input = json!({"version":"2.1","metadata":{"rules_version":"1.2.7"},section:{}});
        let actual = CollectionBuilder::default()
            .update("rules".into(), &input, &semver::Version::new(2, 1, 0))
            .unwrap();
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            json!({
                "ruleset_version":"1.2.7",
                section:{"error":"bad cast, expected 'array', obtained 'map'"}
            })
        );
    }

    #[test]
    fn invalid_scanners_container() {
        invalid_container("scanners");
    }
    #[test]
    fn invalid_processors_container() {
        invalid_container("processors");
    }
}

mod bounded_scanners {
    use super::*;
    use core::cell::Cell;

    struct Counting<'a> {
        input: &'a Input,
        reads: &'a Cell<usize>,
    }

    impl<'a> SchemaSource<'a> for Counting<'a> {
        type Error = &'static str;
        fn kind(&self) -> SourceKind {
            Source(self.input).kind()
        }
        fn element(&self, index: usize) -> Result<Self, Self::Error> {
            Ok(Self {
                input: Source(self.input).element(index).unwrap().0,
                reads: self.reads,
            })
        }
        fn entry(&self, index: usize) -> Result<(Cow<'a, str>, Option<Self>), Self::Error> {
            let (key, value) = Source(self.input).entry(index).unwrap();
            Ok((
                key,
                value.map(|s| Self {
                    input: s.0,
                    reads: self.reads,
                }),
            ))
        }
    }
    impl<'a> ScannerSource<'a> for Counting<'a> {
        fn string(&self) -> Result<Cow<'a, str>, Self::Error> {
            self.reads.set(self.reads.get() + 1);
            let text = Source(self.input).string().unwrap();
            if text == "poison" {
                Err("string access failed")
            } else {
                Ok(text)
            }
        }
    }
    fn scanner(key: Option<&str>, value: Option<&str>) -> Scanner {
        Scanner::new(
            "test".into(),
            BTreeMap::from([("type".into(), "classified".into())]),
            key.map(|v| Matcher::exact(vec![v.into()])),
            value.map(|v| Matcher::exact(vec![v.into()])),
        )
        .unwrap()
    }
    fn read(input: &Input, scanners: &[Scanner]) -> (Result<Value, &'static str>, usize) {
        let reads = Cell::new(0);
        let source = Counting {
            input,
            reads: &reads,
        };
        let result =
            extract_schema_from_source_with_scanners(&source, SchemaOptions::default(), scanners)
                .map(|s| serde_json::to_value(s).unwrap());
        (result, reads.get())
    }
    #[test]
    fn key_rejections_and_key_only_matches_do_not_read_string_contents() {
        let input: Input = serde_json::from_str(r#"{"email":"poison"}"#).unwrap();
        for scanners in [vec![], vec![scanner(Some("other"), Some("value"))]] {
            let (schema, reads) = read(&input, &scanners);
            assert_eq!(schema.unwrap(), json!([{"email":[8]}]));
            assert_eq!(reads, 0);
        }
        let (schema, reads) = read(&input, &[scanner(Some("email"), None)]);
        assert_eq!(
            schema.unwrap(),
            json!([{"email":[8,{"type":"classified"}]}])
        );
        assert_eq!(reads, 0);
    }
    #[test]
    fn selected_string_errors_propagate() {
        let input: Input = serde_json::from_str(r#"{"email":"poison"}"#).unwrap();
        let (error, reads) = read(&input, &[scanner(Some("email"), Some("value"))]);
        assert_eq!(error.unwrap_err(), "string access failed");
        assert_eq!(reads, 1);
    }
    #[test]
    fn multiple_matchers_share_one_string_read() {
        let input: Input = serde_json::from_str(r#"{"email":"value"}"#).unwrap();
        let (schema, reads) = read(
            &input,
            &[
                scanner(Some("email"), Some("miss")),
                scanner(Some("email"), Some("value")),
                scanner(Some("email"), None),
            ],
        );
        assert_eq!(
            schema.unwrap(),
            json!([{"email":[8,{"type":"classified"}]}])
        );
        assert_eq!(reads, 1);
    }
    #[test]
    fn discarded_strings_are_not_read_at_array_record_or_depth_limits() {
        let scanners = [scanner(None, Some("value"))];
        let array = Input::Array(
            (0..11)
                .map(|i| Input::Scalar(json!(if i == 10 { "poison" } else { "value" })))
                .collect(),
        );
        let (schema, reads) = read(&array, &scanners);
        assert!(schema.is_ok());
        assert_eq!(reads, 10);
        let record = Input::Record(
            (0..256)
                .map(|i| {
                    (
                        format!("key-{i}"),
                        Input::Scalar(json!(if i == 255 { "poison" } else { "value" })),
                    )
                })
                .collect(),
        );
        let (schema, reads) = read(&record, &scanners);
        assert!(schema.is_ok());
        assert_eq!(reads, 255);
        let deep = (0..18).fold(Input::Scalar(json!("poison")), |child, _| {
            Input::Array(vec![child])
        });
        let (schema, reads) = read(&deep, &scanners);
        assert!(schema.is_ok());
        assert_eq!(reads, 0);
        let empty_key = Input::Record(vec![(String::new(), Input::Scalar(json!("poison")))]);
        let (schema, reads) = read(&empty_key, &scanners);
        assert_eq!(schema.unwrap(), json!([{}]));
        assert_eq!(reads, 0);
    }
}

#[test]
fn collection_snapshots_survive_updates_and_invalid_roots() {
    use libdd_api_security::CollectionBuilder;
    let cases: Value =
        serde_json::from_str(include_str!("fixtures/libddwaf-integration-overrides.json")).unwrap();
    let steps = cases["cases"][0]["steps"].as_array().unwrap();
    let mut builder = CollectionBuilder::default();
    for step in steps.iter().take_while(|s| s["op"] != "collect") {
        if step["op"] == "update" {
            builder
                .update(
                    step["source"].as_str().unwrap().into(),
                    &cases["configurations"][step["configuration"].as_str().unwrap()],
                    &semver::Version::new(2, 1, 0),
                )
                .unwrap();
        }
    }
    let old = builder.build();
    assert!(
        builder
            .update(
                "processors".into(),
                &Value::Null,
                &semver::Version::new(2, 1, 0)
            )
            .is_err()
    );
    let retained = builder.build();
    assert!(builder.remove("processors"));
    let new = builder.build();
    let input = json!({"email":"employee@company.com"});
    let lookup = |address: &str| (address == "server.request.body").then_some(&input);
    assert_eq!(old.collect(lookup).len(), 1);
    assert_eq!(retained.collect(lookup).len(), 1);
    assert!(new.collect(lookup).is_empty());
}
