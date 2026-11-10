// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The shared dependent-flag contract, including prerequisite telemetry eligibility.

use std::{collections::HashMap, fs, path::PathBuf, sync::Arc};

use chrono::Utc;
use libdd_ffe::rules_based::{
    AssignmentReason, Attribute, Configuration, EvaluationContext, EvaluationError,
    EvaluationOptions, FlagType, Str, UniversalFlagConfig,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    flag: String,
    variation_type: FlagType,
    default_value: Value,
    targeting_key: Option<Str>,
    attributes: HashMap<Str, Attribute>,
    result: Value,
    expectations: Option<Value>,
    #[serde(flatten)]
    selection: Selection,
}

#[derive(Default, Deserialize)]
struct Selection {
    #[serde(default, rename = "_skip")]
    skip: Vec<Value>,
    #[serde(default, rename = "_include")]
    include: Vec<Value>,
}

impl Selection {
    fn applies(&self) -> bool {
        assert!(self.skip.is_empty() || self.include.is_empty());
        !self.skip.iter().any(|entry| entry["platform"] == "rust")
            && (self.include.is_empty()
                || self.include.iter().any(|entry| entry["platform"] == "rust"))
    }
}

fn error_code(error: &EvaluationError) -> &'static str {
    match error {
        EvaluationError::FlagUnrecognizedOrDisabled => "FLAG_NOT_FOUND",
        EvaluationError::ConfigurationParseError | EvaluationError::FlagConfigurationInvalid => {
            "PARSE_ERROR"
        }
        EvaluationError::TargetingKeyMissing => "TARGETING_KEY_MISSING",
        EvaluationError::TypeMismatch { .. } => "TYPE_MISMATCH",
        _ => "GENERAL",
    }
}

fn assert_events(kind: &str, case: &Case, mut actual: Vec<Value>) {
    let Some(expectations) = &case.expectations else {
        return;
    };
    for matcher in expectations[kind].as_array().into_iter().flatten() {
        if !serde_json::from_value::<Selection>(matcher.clone())
            .unwrap()
            .applies()
        {
            continue;
        }
        let fields = matcher.as_object().unwrap();
        let count = matcher["_count"].as_u64().unwrap_or(1);
        for _ in 0..count {
            let matched = actual.iter().position(|event| {
                fields
                    .iter()
                    .all(|(key, value)| key.starts_with('_') || event.get(key) == Some(value))
            });
            let Some(index) = matched else {
                panic!("{}: missing {kind} {matcher}; actual {actual:?}", case.flag);
            };
            actual.remove(index);
        }
    }
    if expectations["noUnmatchedEvents"] == true {
        assert!(
            actual.is_empty(),
            "{}: unexpected {kind} {actual:?}",
            case.flag
        );
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn canonical_dependent_flag_results_and_telemetry() {
    let suite = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("ffe-system-test-data/evaluation-suites/dependent-flags");
    let mut files: Vec<_> = fs::read_dir(suite.join("cases"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    files.sort();
    assert!(!files.is_empty(), "dependent-flag fixtures must run");
    let mut cases = Vec::new();
    for file in files {
        let file_cases: Vec<Case> = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
        cases.extend(
            file_cases
                .into_iter()
                .filter(|case| case.selection.applies()),
        );
    }
    assert_eq!(
        cases.len(),
        14,
        "the pinned fixtures must run all Rust cases"
    );
    let config = Configuration::from_server_response(
        UniversalFlagConfig::from_json(fs::read(suite.join("ufc-config.json")).unwrap()).unwrap(),
    );
    for case in cases {
        let context = EvaluationContext::new(
            case.targeting_key.clone(),
            Arc::new(case.attributes.clone()),
        );
        let trace = config.eval_flag_with_trace(
            &case.flag,
            &context,
            case.variation_type.into(),
            Utc::now(),
            EvaluationOptions::default(),
        );
        let (value, reason, code) = match &trace.result {
            Ok(assignment) => (
                assignment.value.variation_value(),
                match assignment.reason {
                    AssignmentReason::Static => "STATIC",
                    AssignmentReason::TargetingMatch => "TARGETING_MATCH",
                    AssignmentReason::Split => "SPLIT",
                    AssignmentReason::Default => "DEFAULT",
                },
                None,
            ),
            Err(error) => (case.default_value.clone(), "ERROR", Some(error_code(error))),
        };
        assert_eq!(value, case.result["value"], "{}", case.flag);
        if let Some(expected) = case.result["reason"].as_str() {
            assert_eq!(reason, expected, "{}", case.flag);
        }
        if let Some(expected) = case.result["errorCode"].as_str() {
            assert_eq!(code, Some(expected), "{}", case.flag);
        }
        if let Ok(assignment) = &trace.result {
            if let Some(expected) = case.result.get("doLog") {
                assert_eq!(json!(assignment.do_log), *expected, "{}", case.flag);
            }
            if let Some(expected) = case.result.get("serialId") {
                assert_eq!(json!(assignment.serial_id), *expected, "{}", case.flag);
            }
        }
        let mut evaluations: Vec<_> = trace
            .evaluations
            .iter()
            .map(|record| {
                json!({"flag": record.flag_key.as_str(),
                "errorCode": record.result.as_ref().err().map(error_code)})
            })
            .collect();
        evaluations.push(json!({"flag": case.flag, "errorCode": code}));
        assert_events("evaluationEvents", &case, evaluations);

        let mut exposures: Vec<_> = trace.evaluations.iter().filter_map(|record| {
            record.result.as_ref().ok().filter(|assignment| record.exposure_eligible && assignment.do_log)
                .map(|assignment| json!({"flag": record.flag_key.as_str(),
                    "allocation": assignment.allocation_key.as_str(), "variant": assignment.variation_key.as_str()}))
        }).collect();
        if let Ok(assignment) = &trace.result
            && assignment.do_log
        {
            exposures.push(
                json!({"flag": case.flag, "allocation": assignment.allocation_key.as_str(),
                "variant": assignment.variation_key.as_str()}),
            );
        }
        assert_events("exposures", &case, exposures);
    }
}
