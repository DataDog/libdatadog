// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Conformance for the `scale` and `scale-json` evaluation suites of ffe-system-test-data.
//! The `ffe-scale` benchmark measures the same cases.

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::Utc;
use libdd_ffe::rules_based::{
    AssignmentReason, Attribute, Configuration, EvaluationContext, EvaluationError, FeatureValue,
    FlagType, Str, UniversalFlagConfig,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestCase {
    flag: String,
    variation_type: FlagType,
    default_value: serde_json::Value,
    targeting_key: Option<Str>,
    attributes: HashMap<Str, serde_json::Value>,
    result: TestResult,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestResult {
    value: serde_json::Value,
    reason: Option<String>,
    // Present only where a case asserts it; `Some(None)` expects no serial id.
    #[serde(default, deserialize_with = "present")]
    serial_id: Option<Option<i32>>,
    do_log: Option<bool>,
    /// Destination -> feature key -> value.
    features: Option<BTreeMap<String, BTreeMap<String, serde_json::Value>>>,
}

fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn suite_root(suite: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("ffe-system-test-data/evaluation-suites")
        .join(suite)
}

fn reason_from_assignment(reason: AssignmentReason) -> &'static str {
    match reason {
        AssignmentReason::TargetingMatch => "TARGETING_MATCH",
        AssignmentReason::Split => "SPLIT",
        AssignmentReason::Default => "DEFAULT",
        AssignmentReason::Static => "STATIC",
    }
}

fn reason_from_error(err: &EvaluationError) -> &'static str {
    match err {
        EvaluationError::FlagDisabled => "DISABLED",
        EvaluationError::DefaultAllocationNull => "DEFAULT",
        _ => "ERROR",
    }
}

fn feature_json(value: &FeatureValue) -> serde_json::Value {
    match value {
        FeatureValue::String(s) => serde_json::Value::String(s.to_string()),
        FeatureValue::Number(n) => serde_json::json!(n),
        FeatureValue::Boolean(b) => serde_json::Value::Bool(*b),
    }
}

fn check_suite(suite: &str) -> usize {
    let root = suite_root(suite);
    let config = UniversalFlagConfig::from_json(fs::read(root.join("ufc-config.json")).unwrap())
        .unwrap_or_else(|err| panic!("failed to parse {suite}/ufc-config.json: {err}"));
    let config = Configuration::from_server_response(config);
    let now = Utc::now();

    let mut paths: Vec<_> = fs::read_dir(root.join("cases"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    paths.sort();

    let mut count = 0;
    for path in &paths {
        let cases: Vec<TestCase> = serde_json::from_reader(fs::File::open(path).unwrap())
            .unwrap_or_else(|err| panic!("failed to parse {}: {err}", path.display()));
        for (index, case) in cases.into_iter().enumerate() {
            count += 1;
            check_case(&config, now, path, index, case);
        }
    }
    count
}

fn check_case(
    config: &Configuration,
    now: chrono::DateTime<Utc>,
    path: &Path,
    index: usize,
    case: TestCase,
) {
    let at = format!("{} case {index} ({})", path.display(), case.flag);
    let attributes = case
        .attributes
        .into_iter()
        .map(|(key, value)| (key, serde_json::from_value::<Attribute>(value).unwrap()))
        .collect();
    let subject = EvaluationContext::new(case.targeting_key, Arc::new(attributes));
    let result = config.eval_flag(&case.flag, &subject, case.variation_type.into(), now);
    let expected = case.result;

    let assignment = match result {
        Ok(assignment) => assignment,
        Err(err) => {
            assert_eq!(case.default_value, expected.value, "value for {at}");
            if let Some(reason) = expected.reason.as_deref() {
                assert_eq!(reason_from_error(&err), reason, "reason for {at}");
            }
            return;
        }
    };

    // Compare without cloning a multi-megabyte value into a `serde_json::Value`.
    let matches = match assignment.value.as_json() {
        Some(value) => *value == expected.value,
        None => assignment.value.variation_value() == expected.value,
    };
    assert!(matches, "value for {at}");
    if let Some(reason) = expected.reason.as_deref() {
        assert_eq!(
            reason_from_assignment(assignment.reason),
            reason,
            "reason for {at}"
        );
    }
    if let Some(serial_id) = expected.serial_id {
        assert_eq!(assignment.serial_id, serial_id, "serialId for {at}");
    }
    if let Some(do_log) = expected.do_log {
        assert_eq!(assignment.do_log, do_log, "doLog for {at}");
    }
    if let Some(expected_features) = expected.features {
        let mut actual: BTreeMap<String, BTreeMap<String, serde_json::Value>> = expected_features
            .keys()
            .map(|destination| (destination.clone(), BTreeMap::new()))
            .collect();
        for feature in assignment.features_slice() {
            for destination in feature.destinations.iter() {
                actual
                    .entry(destination.to_string())
                    .or_default()
                    .insert(feature.key.to_string(), feature_json(&feature.value));
            }
        }
        assert_eq!(actual, expected_features, "features for {at}");
    }
}

#[test]
#[cfg_attr(miri, ignore)] // this test is too slow on miri
fn evaluates_scale_suite() {
    let count = check_suite("scale");
    assert!(count >= 100, "scale suite coverage was reduced: {count}");
}

#[test]
#[cfg_attr(miri, ignore)] // this test is too slow on miri
fn evaluates_scale_json_suite() {
    let count = check_suite("scale-json");
    assert!(
        count >= 13,
        "scale-json suite coverage was reduced: {count}"
    );
}
