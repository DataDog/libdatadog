// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Datelike, SecondsFormat, TimeDelta, Utc};
use libdd_ffe::rules_based::{
    Assignment, Attribute, Configuration, EvaluationContext, EvaluationError, EvaluationOptions,
    ExpectedFlagType, ExperimentationOptOutMode, Str, UniversalFlagConfig,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct PolicyCase {
    id: String,
    args: PolicyArgs,
    expected: PolicyResult,
}

#[derive(Deserialize)]
struct PolicyArgs {
    mode: String,
    attribute: String,
    activation: String,
    #[serde(rename = "doLog")]
    do_log: bool,
}

#[derive(Deserialize)]
struct PolicyResult {
    error: String,
    exclude: bool,
}

fn flag(activation: Value, do_log: bool) -> Value {
    json!({
        "key": "feature", "enabled": true, "variationType": "STRING",
        "variations": {
            "experiment": {"key": "experiment", "value": "experiment"},
            "ordinary": {"key": "ordinary", "value": "ordinary"}
        },
        "allocations": [{
            "key": "candidate", "doLog": do_log, "randomizationActivatedAt": activation,
            "splits": [{"variationKey": "experiment", "shards": [], "serialId": 7}]
        }, {
            "key": "fallback", "doLog": false,
            "splits": [{"variationKey": "ordinary", "shards": [], "serialId": 8}]
        }]
    })
}

fn configuration(flag: Value) -> Configuration {
    Configuration::from_server_response(
        UniversalFlagConfig::from_json(
            serde_json::to_vec(&json!({
                "createdAt": "2026-01-01T00:00:00Z", "environment": {"name": "test"},
                "flags": {"feature": flag, "neighbor": self::flag(Value::Null, false)}
            }))
            .unwrap(),
        )
        .unwrap(),
    )
}

fn context(attribute: &str) -> EvaluationContext {
    let mut attributes = HashMap::<Str, Attribute>::new();
    match attribute {
        "absent" => {}
        "valid" => {
            attributes.insert(
                "dd_experimentation_opt_out_at".into(),
                "2026-01-01T00:00:00Z".into(),
            );
        }
        "invalid" => {
            attributes.insert("dd_experimentation_opt_out_at".into(), true.into());
        }
        _ => panic!("unknown attribute state: {attribute}"),
    }
    EvaluationContext::new(Some("subject".into()), Arc::new(attributes))
}

fn evaluate(
    config: &Configuration,
    key: &str,
    mode: ExperimentationOptOutMode,
    attribute: &str,
) -> Result<Assignment, EvaluationError> {
    config.eval_flag_with_options(
        key,
        &context(attribute),
        ExpectedFlagType::String,
        Utc::now(),
        EvaluationOptions {
            experimentation_opt_out_mode: mode,
        },
    )
}

const MODES: [ExperimentationOptOutMode; 3] = [
    ExperimentationOptOutMode::Disabled,
    ExperimentationOptOutMode::Immediate,
    ExperimentationOptOutMode::FutureExperimentsOnly,
];

#[test]
fn lean_policy_cases_use_production_evaluator() {
    // D002/D004 r2/D010/D011. Expectations are copied unchanged from the Lean oracle.
    let mut count = 0;
    for line in include_str!("data/experimentation-opt-out-policy.jsonl").lines() {
        let case: PolicyCase = serde_json::from_str(line).unwrap();
        let mode = match case.args.mode.as_str() {
            "disabled" => MODES[0],
            "immediate" => MODES[1],
            "future_experiments_only" => MODES[2],
            _ => panic!("unknown mode: {}", case.id),
        };
        let activation = match case.args.activation.as_str() {
            "missing" => Value::Null,
            "invalid" => json!("invalid"),
            "before" => json!("2025-12-31T23:59:59.999999999Z"),
            "equal" => json!("2026-01-01T00:00:00Z"),
            "after" => json!("2026-01-01T00:00:00.000000001Z"),
            _ => panic!("unknown activation: {}", case.id),
        };
        let config = configuration(flag(activation, case.args.do_log));
        let result = evaluate(&config, "feature", mode, &case.args.attribute);
        match case.expected.error.as_str() {
            "INVALID_CONTEXT" => assert_eq!(
                result.unwrap_err(),
                EvaluationError::InvalidContext,
                "{}",
                case.id
            ),
            "PARSE_ERROR" => assert_eq!(
                result.unwrap_err(),
                EvaluationError::FlagConfigurationInvalid,
                "{}",
                case.id
            ),
            "NONE" => {
                let assignment = result.unwrap_or_else(|e| panic!("{}: {e}", case.id));
                assert_eq!(
                    assignment.allocation_key.as_str(),
                    if case.expected.exclude {
                        "fallback"
                    } else {
                        "candidate"
                    },
                    "{}",
                    case.id
                );
                assert_eq!(
                    assignment.value.variation_value(),
                    json!(if case.expected.exclude {
                        "ordinary"
                    } else {
                        "experiment"
                    }),
                    "{}",
                    case.id
                );
                assert_eq!(
                    assignment.variation_key.as_str(),
                    if case.expected.exclude {
                        "ordinary"
                    } else {
                        "experiment"
                    },
                    "{}",
                    case.id
                );
                assert_eq!(
                    assignment.serial_id,
                    Some(if case.expected.exclude { 8 } else { 7 }),
                    "{}",
                    case.id
                );
                assert_eq!(
                    assignment.do_log,
                    !case.expected.exclude && case.args.do_log,
                    "{}",
                    case.id
                );
            }
            _ => panic!("unknown expected error: {}", case.id),
        }
        count += 1;
    }
    assert_eq!(count, 90, "complete Lean policy profile must execute");
    eprintln!("Lean policy: {count} passed, 0 failed, 0 skipped");
}

#[test]
fn activation_validation_covers_unreachable_and_ordinary_allocations() {
    // D004 applies to the entire flag, even after a matching allocation or behind time/rule gates.
    for malformed in [
        json!("invalid"),
        json!(true),
        json!(42),
        json!([]),
        json!({}),
    ] {
        for position in [
            "after-match",
            "unmatched-rule",
            "future-start",
            "expired",
            "ordinary",
        ] {
            let mut wire = flag(malformed.clone(), position != "ordinary");
            match position {
                "after-match" => wire["allocations"].as_array_mut().unwrap().swap(0, 1),
                "unmatched-rule" => {
                    wire["allocations"][0]["rules"] = json!([{"conditions": [{"attribute": "missing", "operator": "ONE_OF", "value": ["required"]}]}])
                }
                "future-start" => wire["allocations"][0]["startAt"] = json!("9999-01-01T00:00:00Z"),
                "expired" => wire["allocations"][0]["endAt"] = json!("2000-01-01T00:00:00Z"),
                _ => {}
            }
            let config = configuration(wire);
            for mode in MODES {
                for attribute in ["absent", "valid", "invalid"] {
                    let result = evaluate(&config, "feature", mode, attribute);
                    if mode != MODES[0] && attribute == "invalid" {
                        assert_eq!(result.unwrap_err(), EvaluationError::InvalidContext);
                    } else if mode == MODES[2] && attribute == "valid" {
                        assert_eq!(
                            result.unwrap_err(),
                            EvaluationError::FlagConfigurationInvalid,
                            "{position}: {malformed}"
                        );
                    } else {
                        assert!(
                            result.is_ok(),
                            "{mode:?}/{attribute}/{position}: {malformed}: {result:?}"
                        );
                    }
                    if attribute != "invalid" || mode == MODES[0] {
                        assert!(evaluate(&config, "neighbor", mode, attribute).is_ok());
                    }
                }
            }
        }
    }
}

#[test]
fn start_and_end_validity_remain_independent_of_opt_out() {
    for field in ["startAt", "endAt"] {
        let mut wire = flag(Value::Null, false);
        wire["allocations"][0][field] = json!("invalid");
        let config = configuration(wire);
        for mode in MODES {
            for attribute in ["absent", "valid"] {
                assert_eq!(
                    evaluate(&config, "feature", mode, attribute).unwrap_err(),
                    EvaluationError::FlagConfigurationInvalid
                );
                assert!(evaluate(&config, "neighbor", mode, attribute).is_ok());
            }
        }
    }
}

#[test]
fn missing_and_null_activation_have_the_same_conservative_behavior() {
    for missing in [true, false] {
        let mut wire = flag(Value::Null, true);
        if missing {
            wire["allocations"][0]
                .as_object_mut()
                .unwrap()
                .remove("randomizationActivatedAt");
        }
        let config = configuration(wire);
        let result = evaluate(&config, "feature", MODES[2], "valid").unwrap();
        assert_eq!(result.allocation_key.as_str(), "fallback");
        assert!(!result.do_log);
    }
}

#[test]
fn opt_out_grammar_does_not_narrow_allocation_schedules() {
    for (field, timestamp) in [
        ("startAt", "2000-01-01 00:00:00Z"),
        ("endAt", "9999-01-01 00:00:00Z"),
    ] {
        let mut wire = flag(Value::Null, false);
        wire["allocations"][0][field] = json!(timestamp);
        let config = configuration(wire);
        for mode in MODES {
            let assignment = evaluate(&config, "feature", mode, "valid").unwrap();
            assert_eq!(assignment.allocation_key.as_str(), "candidate", "{field}");
        }
    }
}

#[test]
fn opt_out_timestamps_reject_surrounding_whitespace() {
    for timestamp in [
        " 2026-01-01T00:00:00Z",
        "2026-01-01T00:00:00Z ",
        "\t2026-01-01T00:00:00Z",
        "2026-01-01T00:00:00Z\n",
        "\u{2003}2026-01-01T00:00:00Z",
    ] {
        let config = configuration(flag(json!(timestamp), true));
        for mode in MODES {
            let subject = EvaluationContext::new(
                Some("subject".into()),
                Arc::new(HashMap::from([(
                    "dd_experimentation_opt_out_at".into(),
                    timestamp.into(),
                )])),
            );
            let result = config.eval_flag_with_options(
                "missing",
                &subject,
                ExpectedFlagType::String,
                Utc::now(),
                EvaluationOptions {
                    experimentation_opt_out_mode: mode,
                },
            );
            assert_eq!(
                result.unwrap_err(),
                if mode == MODES[0] {
                    EvaluationError::FlagUnrecognizedOrDisabled
                } else {
                    EvaluationError::InvalidContext
                }
            );
            let activation = evaluate(&config, "feature", mode, "valid");
            if mode == MODES[2] {
                assert_eq!(
                    activation.unwrap_err(),
                    EvaluationError::FlagConfigurationInvalid
                );
            } else {
                assert!(activation.is_ok());
            }
            assert!(evaluate(&config, "feature", mode, "absent").is_ok());
            assert!(evaluate(&config, "neighbor", mode, "valid").is_ok());
        }
    }
}

#[derive(Deserialize)]
struct TimestampCase {
    id: String,
    input: Value,
    expected: TimestampResult,
}

#[derive(Deserialize)]
struct TimestampResult {
    accepted: bool,
    seconds: Option<i64>,
    nanos: Option<u32>,
}

fn evaluate_timestamp(
    config: &Configuration,
    cutoff: &Value,
    mode: ExperimentationOptOutMode,
) -> Result<Assignment, EvaluationError> {
    let subject = EvaluationContext::new(
        Some("subject".into()),
        Arc::new(HashMap::from([(
            "dd_experimentation_opt_out_at".into(),
            serde_json::from_value(cutoff.clone()).unwrap(),
        )])),
    );
    config.eval_flag_with_options(
        "feature",
        &subject,
        ExpectedFlagType::String,
        Utc::now(),
        EvaluationOptions {
            experimentation_opt_out_mode: mode,
        },
    )
}

fn assert_timestamp_assignment(result: Assignment, retained: bool, id: &str) {
    assert_eq!(
        result.allocation_key.as_str(),
        if retained { "candidate" } else { "fallback" },
        "{id}"
    );
    assert_eq!(result.do_log, retained, "{id}");
    assert_eq!(result.serial_id, Some(if retained { 7 } else { 8 }), "{id}");
}

#[test]
fn lean_timestamp_cases_use_both_production_paths() {
    // D005/D006/D007: frozen expected instants, never the host parser, are the oracle.
    let cases: Vec<TimestampCase> = include_str!("data/experimentation-opt-out-timestamps.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(cases.len(), 50, "complete timestamp profile must execute");
    let mut evaluations = 0;
    for case in &cases {
        let activation_config = configuration(flag(case.input.clone(), true));
        if case.expected.accepted {
            let instant = DateTime::from_timestamp(
                case.expected.seconds.unwrap(),
                case.expected.nanos.unwrap(),
            )
            .unwrap();
            for delta in [-1, 0, 1] {
                let adjacent = instant + TimeDelta::nanoseconds(delta);
                // The UTC boundary itself is covered; its out-of-domain neighbor is not a
                // valid reference timestamp. Every corpus case still executes both paths.
                if !(0..=9999).contains(&adjacent.year()) {
                    continue;
                }
                let reference = json!(adjacent.to_rfc3339_opts(SecondsFormat::Nanos, true));
                let cutoff_config = configuration(flag(reference.clone(), true));
                assert_timestamp_assignment(
                    evaluate_timestamp(&cutoff_config, &case.input, MODES[2])
                        .unwrap_or_else(|error| panic!("{} cutoff: {error}", case.id)),
                    delta < 0,
                    &case.id,
                );
                assert_timestamp_assignment(
                    evaluate_timestamp(&activation_config, &reference, MODES[2])
                        .unwrap_or_else(|error| panic!("{} activation: {error}", case.id)),
                    delta > 0,
                    &case.id,
                );
                evaluations += 2;
            }
        } else {
            let valid_config = configuration(flag(json!("2025-01-01T00:00:00Z"), true));
            for mode in MODES {
                let cutoff = evaluate_timestamp(&valid_config, &case.input, mode);
                if mode == MODES[0] {
                    assert_timestamp_assignment(cutoff.unwrap(), true, &case.id);
                } else {
                    assert_eq!(
                        cutoff.err(),
                        Some(EvaluationError::InvalidContext),
                        "{} cutoff: {mode:?}",
                        case.id
                    );
                }
                let activation = evaluate(&activation_config, "feature", mode, "valid");
                if mode == MODES[2] && !case.input.is_null() {
                    assert_eq!(
                        activation.err(),
                        Some(EvaluationError::FlagConfigurationInvalid),
                        "{} activation",
                        case.id
                    );
                } else {
                    // Null remains missing activation, even though it is invalid subject input.
                    assert_timestamp_assignment(activation.unwrap(), mode == MODES[0], &case.id);
                }
                assert_timestamp_assignment(
                    evaluate(&activation_config, "feature", mode, "absent").unwrap(),
                    true,
                    &case.id,
                );
                assert!(evaluate(&activation_config, "neighbor", mode, "valid").is_ok());
                evaluations += 4;
            }
        }
    }
    eprintln!(
        "Lean timestamp profile: {} cases, {evaluations} evaluations, 0 failed, 0 excluded cases",
        cases.len()
    );
}
