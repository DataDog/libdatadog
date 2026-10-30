// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::eval_assignment::*;
use crate::rules_based::{
    Configuration, EvaluationContext, EvaluationError, ExpectedFlagType, UniversalFlagConfig,
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};

fn allocation(conditions: Vec<Value>, variant: &str) -> Value {
    json!({"key": variant, "rules": [{"conditions": conditions}], "doLog": true,
        "splits": [{"shards": [], "variationKey": variant}]})
}

fn flag(conditions: Vec<Value>) -> Value {
    json!({"key": "ignored", "enabled": true, "variationType": "BOOLEAN",
        "variations": {"on": {"key": "on", "value": false}, "off": {"key": "off", "value": true}},
        "allocations": [allocation(conditions, "on"), allocation(vec![], "off")]})
}

fn dependency(key: &str) -> Value {
    json!({"flagEvaluation": {"key": key}, "operator": "ONE_OF", "value": ["on"]})
}

fn fails() -> Value {
    json!({"attribute": "country", "operator": "ONE_OF", "value": ["FR"]})
}

fn wire(flags: Value) -> Value {
    json!({"createdAt": "2026-10-28T00:00:00Z", "environment": {"name": "test"}, "flags": flags})
}

fn eval(wire: Value) -> EvaluationResult {
    evaluate(
        wire,
        EvaluationOptions::default(),
        EvaluationContext::new(Some("subject".into()), Default::default()),
    )
}

fn evaluate(
    wire: Value,
    options: EvaluationOptions,
    context: EvaluationContext,
) -> EvaluationResult {
    let config = Configuration::from_server_response(
        UniversalFlagConfig::from_json(serde_json::to_vec(&wire).unwrap()).unwrap(),
    );
    config.eval_flag_with_trace(
        "root",
        &context,
        ExpectedFlagType::Boolean,
        "2026-10-28T00:00:00Z".parse::<DateTime<Utc>>().unwrap(),
        options,
    )
}

fn cause(error: &EvaluationError) -> &EvaluationError {
    match error {
        EvaluationError::DependencyEvaluationFailed { source, .. } => cause(source),
        error => error,
    }
}

#[test]
fn compares_variant_keys_not_values_and_supports_both_membership_operators() {
    for (operator, values, expected) in [
        ("ONE_OF", json!(["on"]), "on"),
        ("ONE_OF", json!(["false"]), "off"),
        ("NOT_ONE_OF", json!(["off"]), "on"),
        ("NOT_ONE_OF", json!(["on"]), "off"),
    ] {
        let condition =
            json!({"flagEvaluation": {"key": "parent"}, "operator": operator, "value": values});
        let result = eval(wire(
            json!({"root": flag(vec![condition]), "parent": flag(vec![])}),
        ));
        assert_eq!(result.result.unwrap().variation_key.as_str(), expected);
        assert_eq!(result.evaluations.len(), 1);
        assert_eq!(result.evaluations[0].exposure_eligible, expected == "on");
    }
}

#[test]
fn authored_order_short_circuits_missing_prerequisites() {
    let result = eval(wire(
        json!({"root": flag(vec![fails(), dependency("missing")])}),
    ));
    assert_eq!(result.result.unwrap().variation_key.as_str(), "off");
    assert!(result.evaluations.is_empty());
    let result = eval(wire(
        json!({"root": flag(vec![dependency("missing"), fails()])}),
    ));
    assert!(matches!(
        result.result,
        Err(EvaluationError::DependencyEvaluationFailed { .. })
    ));
    assert_eq!(
        cause(result.result.as_ref().unwrap_err()),
        &EvaluationError::FlagUnrecognizedOrDisabled
    );
    assert_eq!(result.evaluations.len(), 1);
}

#[test]
fn reached_failed_prerequisites_abort_the_root_instead_of_selecting_fallback() {
    let mut disabled = flag(vec![]);
    disabled["enabled"] = json!(false);
    let mut unassigned = flag(vec![]);
    unassigned["allocations"] = json!([]);
    for (parent, expected) in [
        (disabled, EvaluationError::FlagDisabled),
        (unassigned, EvaluationError::DefaultAllocationNull),
        (json!({}), EvaluationError::FlagConfigurationInvalid),
    ] {
        let result = eval(wire(
            json!({"root": flag(vec![dependency("parent")]), "parent": parent}),
        ));
        assert_eq!(cause(result.result.as_ref().unwrap_err()), &expected);
        assert_eq!(
            result.evaluations[0].result.as_ref().unwrap_err(),
            &expected
        );
        assert!(!result.evaluations[0].exposure_eligible);
    }
}

#[test]
fn invalid_dependency_conditions_are_isolated_to_the_containing_flag() {
    for invalid in [
        json!({"flagEvaluation": {"key": "parent"}, "operator": "ONE_OF", "value": []}),
        json!({"flagEvaluation": {"key": "parent"}, "operator": "NOT_ONE_OF", "value": []}),
        json!({"flagEvaluation": {"key": "parent"}, "attribute": "country", "operator": "ONE_OF", "value": ["on"]}),
        json!({"flagEvaluation": {"key": "parent"}, "attribute": "", "operator": "ONE_OF", "value": ["on"]}),
        json!({"flagEvaluation": {"key": "parent", "unexpected": true}, "operator": "ONE_OF", "value": ["on"]}),
        json!({"flagEvaluation": {"key": ""}, "operator": "ONE_OF", "value": ["on"]}),
        json!({"flagEvaluation": {"key": "parent"}, "operator": "IS_NULL", "value": false}),
        json!({"flagEvaluation": {"key": "parent"}, "operator": "ONE_OF", "value": "on"}),
        json!({"flagEvaluation": {"key": "parent"}, "operator": "ONE_OF", "value": [true]}),
    ] {
        let w = wire(json!({"root": flag(vec![invalid]), "parent": flag(vec![])}));
        assert_eq!(
            eval(w.clone()).result.unwrap_err(),
            EvaluationError::FlagConfigurationInvalid
        );
        let config = Configuration::from_server_response(
            UniversalFlagConfig::from_json(serde_json::to_vec(&w).unwrap()).unwrap(),
        );
        assert!(
            config
                .eval_flag(
                    "parent",
                    &EvaluationContext::new(None, Default::default()),
                    ExpectedFlagType::Any,
                    Utc::now()
                )
                .is_ok()
        );
    }
}

#[test]
fn dnf_expansion_memoizes_but_restores_exposure_eligibility_for_the_winning_rule() {
    let mut root = flag(vec![]);
    root["allocations"][0]["rules"] = json!([
        {"conditions": [dependency("parent"), fails()]},
        {"conditions": [dependency("parent")]}
    ]);
    let result = eval(wire(json!({"root": root, "parent": flag(vec![])})));
    assert_eq!(result.result.unwrap().variation_key.as_str(), "on");
    assert_eq!(result.evaluations.len(), 1);
    assert!(result.evaluations[0].exposure_eligible);
}

#[test]
fn rejected_rules_retain_evaluations_without_exposures() {
    let result = eval(wire(
        json!({"root": flag(vec![dependency("parent"), fails()]), "parent": flag(vec![])}),
    ));
    assert_eq!(result.result.unwrap().variation_key.as_str(), "off");
    assert_eq!(result.evaluations.len(), 1);
    assert!(result.evaluations[0].result.is_ok());
    assert!(!result.evaluations[0].exposure_eligible);
}

#[test]
fn rejected_split_does_not_expose_prerequisites() {
    let mut root = flag(vec![dependency("parent")]);
    root["allocations"][0]["splits"][0]["shards"] =
        json!([{"salt":"salt", "totalShards":100,"ranges":[]}]);
    let result = eval(wire(json!({"root": root, "parent": flag(vec![])})));
    assert_eq!(result.result.unwrap().variation_key.as_str(), "off");
    assert!(!result.evaluations[0].exposure_eligible);
}

#[test]
fn root_failure_suppresses_all_prerequisite_exposures() {
    let result = eval(wire(
        json!({"root": flag(vec![dependency("parent"), dependency("missing")]), "parent": flag(vec![])}),
    ));
    assert!(result.result.is_err());
    assert_eq!(result.evaluations.len(), 2);
    assert!(result.evaluations.iter().all(|r| !r.exposure_eligible));
}

#[test]
fn shared_transitive_dependencies_are_reported_once_and_keep_do_log() {
    let mut parent = flag(vec![dependency("leaf")]);
    parent["allocations"][0]["doLog"] = json!(false);
    let result = eval(wire(
        json!({"root": flag(vec![dependency("a"), dependency("b")]),
        "a": parent, "b": flag(vec![dependency("leaf")]), "leaf": flag(vec![])}),
    ));
    assert!(result.result.is_ok());
    assert_eq!(
        result
            .evaluations
            .iter()
            .map(|r| r.flag_key.as_str())
            .collect::<Vec<_>>(),
        ["leaf", "a", "b"]
    );
    assert!(result.evaluations.iter().all(|r| r.exposure_eligible));
    assert!(!result.evaluations[1].result.as_ref().unwrap().do_log);
}

#[test]
fn rejected_transitive_paths_never_gain_exposure_eligibility() {
    let result = eval(wire(json!({"root": flag(vec![dependency("a")]),
        "a": flag(vec![dependency("leaf"), fails()]), "leaf": flag(vec![])})));
    assert_eq!(result.result.unwrap().variation_key.as_str(), "off");
    assert!(result.evaluations.iter().all(|r| !r.exposure_eligible));
}

#[test]
fn cycles_fail_without_evaluating_the_root_twice() {
    for flags in [
        json!({"root": flag(vec![dependency("root")])}),
        json!({"root": flag(vec![dependency("parent")]), "parent": flag(vec![dependency("root")])}),
    ] {
        let result = eval(wire(flags));
        assert_eq!(
            cause(result.result.as_ref().unwrap_err()),
            &EvaluationError::DependencyCycle
        );
        assert!(
            result
                .evaluations
                .iter()
                .all(|r| r.flag_key.as_str() != "root" && !r.exposure_eligible)
        );
    }
}

fn chain(edges: usize) -> Value {
    let mut flags = serde_json::Map::new();
    for n in 0..=edges {
        let key = if n == 0 {
            "root".to_owned()
        } else {
            format!("f{n}")
        };
        flags.insert(
            key,
            flag(if n < edges {
                vec![dependency(&format!("f{}", n + 1))]
            } else {
                vec![]
            }),
        );
    }
    wire(flags.into())
}

#[test]
fn depth_counts_edges_defaults_to_ten_and_preserves_explicit_zero() {
    for (edges, limit, passes) in [
        (0, Some(0), true),
        (1, Some(0), false),
        (1, Some(1), true),
        (2, Some(1), false),
        (10, None, true),
        (11, None, false),
        (255, Some(255), true),
        (256, Some(255), false),
    ] {
        let mut w = chain(edges);
        if let Some(limit) = limit {
            w["evaluatorParams"] = json!({"maxDependencyDepth": limit});
        }
        let result = eval(w);
        assert_eq!(
            result.result.is_ok(),
            passes,
            "edges={edges} limit={limit:?}"
        );
        if !passes {
            assert_eq!(
                cause(result.result.as_ref().unwrap_err()),
                &EvaluationError::DependencyDepthExceeded
            );
        }
    }
}

#[test]
fn depth_failures_record_the_terminal_dependency_and_ancestors() {
    let mut w = chain(1);
    w["evaluatorParams"] = json!({"maxDependencyDepth": 0});
    let result = eval(w);
    assert_eq!(
        cause(result.result.as_ref().unwrap_err()),
        &EvaluationError::DependencyDepthExceeded
    );
    assert_eq!(result.evaluations.len(), 1);
    assert_eq!(result.evaluations[0].flag_key.as_str(), "f1");
    assert_eq!(
        result.evaluations[0].result.as_ref().unwrap_err(),
        &EvaluationError::DependencyDepthExceeded
    );

    let mut w = chain(2);
    w["evaluatorParams"] = json!({"maxDependencyDepth": 1});
    let result = eval(w);
    assert_eq!(
        cause(result.result.as_ref().unwrap_err()),
        &EvaluationError::DependencyDepthExceeded
    );
    assert_eq!(result.evaluations.len(), 2);
    assert!(result.evaluations.iter().any(|record| {
        record.flag_key.as_str() == "f2"
            && record.result.as_ref().unwrap_err() == &EvaluationError::DependencyDepthExceeded
    }));
    assert!(result.evaluations.iter().any(|record| {
        record.flag_key.as_str() == "f1"
            && matches!(
                record.result.as_ref().unwrap_err(),
                EvaluationError::DependencyEvaluationFailed { flag_key, source }
                    if flag_key.as_str() == "f2"
                        && **source == EvaluationError::DependencyDepthExceeded
            )
    }));
    assert!(
        result
            .evaluations
            .iter()
            .all(|record| !record.exposure_eligible)
    );
}

#[test]
fn rejects_invalid_depth_limits_and_preserves_wire_parameters() {
    for limit in [json!(-1), json!(256), json!(1.5), json!("10"), json!(true)] {
        let mut w = chain(0);
        w["evaluatorParams"] = json!({"maxDependencyDepth":limit});
        assert!(UniversalFlagConfig::from_json(serde_json::to_vec(&w).unwrap()).is_err());
    }
    let mut w = chain(0);
    w["evaluatorParams"] = json!({"maxDependencyDepth":0});
    let bytes = serde_json::to_vec(&w).unwrap();
    assert_eq!(
        UniversalFlagConfig::from_json(bytes.clone())
            .unwrap()
            .to_json(),
        bytes
    );
}

#[test]
fn memoization_cannot_bypass_depth_on_a_deeper_path_even_for_rejected_rules() {
    for rejected in [false, true] {
        let mut first = vec![dependency("leaf")];
        if rejected {
            first.push(fails());
        }
        let mut conditions = vec![dependency("a")];
        if rejected {
            conditions[0]["value"] = json!(["off"]);
        }
        conditions.push(dependency("b"));
        let mut w = wire(json!({"root": flag(conditions), "a": flag(first),
            "b": flag(vec![dependency("a")]), "leaf":flag(vec![])}));
        w["evaluatorParams"] = json!({"maxDependencyDepth":2});
        let result = eval(w);
        assert_eq!(
            cause(result.result.as_ref().unwrap_err()),
            &EvaluationError::DependencyDepthExceeded
        );
    }
}

#[test]
fn memoization_is_scoped_to_each_top_level_evaluation() {
    let config = Configuration::from_server_response(
        UniversalFlagConfig::from_json(
            serde_json::to_vec(&wire(json!({
        "root": flag(vec![dependency("parent")]), "parent":flag(vec![fails()])})))
            .unwrap(),
        )
        .unwrap(),
    );
    for country in ["FR", "US"] {
        let context = EvaluationContext::new(
            Some("same-subject".into()),
            Arc::new(HashMap::from([("country".into(), country.into())])),
        );
        let result = config.eval_flag_with_trace(
            "root",
            &context,
            ExpectedFlagType::Boolean,
            Utc::now(),
            EvaluationOptions::default(),
        );
        assert_eq!(
            result.result.unwrap().variation_key.as_str(),
            if country == "FR" { "on" } else { "off" }
        );
        assert_eq!(result.evaluations.len(), 1);
    }
}

#[test]
fn dependencies_reuse_context_timestamp_and_experimentation_policy() {
    let mut parent = flag(vec![
        json!({"attribute":"country","operator":"ONE_OF","value":["FR"]}),
    ]);
    parent["allocations"][0]["startAt"] = json!("2026-10-27T00:00:00Z");
    parent["allocations"][0]["endAt"] = json!("2026-10-29T00:00:00Z");
    parent["allocations"][1]["doLog"] = json!(false);
    let mut root = flag(vec![dependency("parent")]);
    root["allocations"][0]["doLog"] = json!(false);
    root["allocations"][1]["doLog"] = json!(false);
    let w = wire(json!({"root":root,"parent":parent}));
    for (mode, expected) in [
        (ExperimentationOptOutMode::Disabled, "on"),
        (ExperimentationOptOutMode::Immediate, "off"),
        (ExperimentationOptOutMode::FutureExperimentsOnly, "off"),
    ] {
        let context = EvaluationContext::new(
            Some("subject".into()),
            Arc::new(HashMap::from([
                ("country".into(), "FR".into()),
                (
                    "dd_experimentation_opt_out_at".into(),
                    "2026-10-26T00:00:00Z".into(),
                ),
            ])),
        );
        let result = evaluate(
            w.clone(),
            EvaluationOptions {
                experimentation_opt_out_mode: mode,
            },
            context,
        );
        assert_eq!(result.result.unwrap().variation_key.as_str(), expected);
    }
}

#[test]
fn later_allocations_still_evaluate_dependencies() {
    let mut root = flag(vec![dependency("parent")]);
    root["allocations"]
        .as_array_mut()
        .unwrap()
        .insert(0, allocation(vec![fails()], "off"));
    let result = eval(wire(json!({"root": root, "parent": flag(vec![])})));
    assert_eq!(result.result.unwrap().variation_key.as_str(), "on");
    assert_eq!(result.evaluations.len(), 1);
    assert!(result.evaluations[0].exposure_eligible);
}

#[test]
fn independent_flags_ignore_unrelated_dependency_graphs() {
    let mut w = wire(json!({"root":flag(vec![]), "cycle":flag(vec![dependency("cycle")])}));
    w["evaluatorParams"] = json!({"maxDependencyDepth":0});
    let result = eval(w);
    assert_eq!(result.result.unwrap().variation_key.as_str(), "on");
    assert!(result.evaluations.is_empty());
}
