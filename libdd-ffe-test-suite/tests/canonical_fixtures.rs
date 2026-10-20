// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashMap, fs, path::PathBuf, sync::Arc};

use chrono::Utc;
use libdd_ffe::rules_based::{
    AssignmentReason, Attribute, Configuration, EvaluationContext, EvaluationError,
    EvaluationOptions, FlagType, Str, UniversalFlagConfig, get_assignment,
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
    #[serde(default)]
    capabilities: HashMap<String, TestResult>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestResult {
    value: serde_json::Value,
    reason: Option<String>,
    error_code: Option<String>,
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ffe-system-test-data")
}

#[test]
#[cfg_attr(miri, ignore)] // this test is too slow on miri
fn evaluates_canonical_json_fixtures() {
    let _ = env_logger::builder().is_test(true).try_init();

    let root = fixture_root();
    let config_path = root.join("ufc-config.json");
    let cases_dir = root.join("evaluation-cases");

    let config = UniversalFlagConfig::from_json(fs::read(&config_path).unwrap()).unwrap();
    let config = Configuration::from_server_response(config);
    let now = Utc::now();

    let mut fixture_count = 0;
    let mut baseline_count = 0;
    let mut opt_out_count = 0;
    for entry in fs::read_dir(&cases_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }

        fixture_count += 1;
        let test_cases: Vec<TestCase> = serde_json::from_reader(fs::File::open(&path).unwrap())
            .unwrap_or_else(|err| panic!("failed to parse fixture {}: {err}", path.display()));

        for test_case in test_cases {
            baseline_count += 1;
            let attributes = test_case
                .attributes
                .into_iter()
                .map(|(key, value)| {
                    // Attribute has no container variants. Preserve this reserved input as
                    // present but invalid; SDK bridge tests cover the original container.
                    let value = if key.as_str() == "dd_experimentation_opt_out_at"
                        && (value.is_array() || value.is_object())
                    {
                        serde_json::Value::Null
                    } else {
                        value
                    };
                    (key, serde_json::from_value::<Attribute>(value).unwrap())
                })
                .collect();
            let subject = EvaluationContext::new(test_case.targeting_key, Arc::new(attributes));
            if let Some(expected) = test_case.capabilities.get("experimentationOptOutImmediate") {
                opt_out_count += 1;
                let result = config.eval_flag_with_options(
                    &test_case.flag,
                    &subject,
                    test_case.variation_type.into(),
                    now,
                    EvaluationOptions {
                        experimentation_opt_out_immediate: true,
                    },
                );
                let (value, reason, error) = match result {
                    Ok(assignment) => (
                        assignment.value.variation_value(),
                        reason_from_assignment(assignment.reason),
                        None,
                    ),
                    Err(err) => (
                        test_case.default_value.clone(),
                        reason_from_error(&err),
                        error_code_from_error(&err),
                    ),
                };
                assert_eq!(
                    value,
                    expected.value,
                    "immediate value for {} in {}",
                    test_case.flag,
                    path.display()
                );
                if let Some(expected_reason) = expected.reason.as_deref() {
                    assert_eq!(
                        reason,
                        expected_reason,
                        "immediate reason for {} in {}",
                        test_case.flag,
                        path.display()
                    );
                }
                assert_eq!(
                    error,
                    expected.error_code.as_deref(),
                    "immediate error for {} in {}",
                    test_case.flag,
                    path.display()
                );
            }
            let result = get_assignment(
                Some(&config),
                &test_case.flag,
                &subject,
                test_case.variation_type.into(),
                now,
            );

            let (actual, actual_reason, actual_error_code) = match result {
                Ok(assignment) => (
                    assignment.value.variation_value(),
                    reason_from_assignment(assignment.reason),
                    None,
                ),
                Err(err) => (
                    test_case.default_value.clone(),
                    reason_from_error(&err),
                    error_code_from_error(&err),
                ),
            };

            assert_eq!(
                actual,
                test_case.result.value,
                "unexpected value for flag {} in {}",
                test_case.flag,
                path.display()
            );

            if let Some(expected_reason) = test_case.result.reason.as_deref() {
                assert_eq!(
                    actual_reason,
                    expected_reason,
                    "unexpected reason for flag {} in {}",
                    test_case.flag,
                    path.display()
                );
            }

            if let Some(expected_error_code) = test_case.result.error_code.as_deref() {
                assert_eq!(
                    actual_error_code,
                    Some(expected_error_code),
                    "unexpected error code for flag {} in {}",
                    test_case.flag,
                    path.display()
                );
            }
        }
    }

    assert!(fixture_count > 0, "no canonical FFE fixtures loaded");
    assert!(
        baseline_count >= 325,
        "baseline fixture coverage was reduced"
    );
    assert!(
        opt_out_count > 0,
        "immediate opt-out capability was not exercised"
    );
    eprintln!("canonical fixtures: {baseline_count} baseline, {opt_out_count} immediate");
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

fn error_code_from_error(err: &EvaluationError) -> Option<&'static str> {
    match err {
        EvaluationError::FlagDisabled | EvaluationError::DefaultAllocationNull => None,
        EvaluationError::TypeMismatch { .. } => Some("TYPE_MISMATCH"),
        EvaluationError::TargetingKeyMissing => Some("TARGETING_KEY_MISSING"),
        EvaluationError::InvalidContext => Some("INVALID_CONTEXT"),
        EvaluationError::ConfigurationParseError | EvaluationError::FlagConfigurationInvalid => {
            Some("PARSE_ERROR")
        }
        EvaluationError::ConfigurationMissing => Some("PROVIDER_NOT_READY"),
        EvaluationError::FlagUnrecognizedOrDisabled => Some("FLAG_NOT_FOUND"),
        EvaluationError::Internal(_) => Some("GENERAL"),
        _ => Some("GENERAL"),
    }
}
