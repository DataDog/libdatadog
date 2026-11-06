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
        ("ONE_OF", json!([]), "off"),
        ("NOT_ONE_OF", json!([]), "on"),
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
fn empty_dependency_comparands_still_evaluate_the_prerequisite() {
    for operator in ["ONE_OF", "NOT_ONE_OF"] {
        let condition =
            json!({"flagEvaluation": {"key": "missing"}, "operator": operator, "value": []});
        let result = eval(wire(json!({"root": flag(vec![condition])})));
        assert_eq!(
            cause(result.result.as_ref().unwrap_err()),
            &EvaluationError::FlagUnrecognizedOrDisabled
        );
        assert_eq!(result.evaluations.len(), 1);
        assert_eq!(result.evaluations[0].flag_key.as_str(), "missing");
        assert!(!result.evaluations[0].exposure_eligible);
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
        json!({"flagEvaluation": {"key": "parent"}, "attribute": "country", "operator": "ONE_OF", "value": ["on"]}),
        json!({"flagEvaluation": {"key": "parent"}, "attribute": "", "operator": "ONE_OF", "value": ["on"]}),
        json!({"flagEvaluation": {"key": "parent"}, "attribute": null, "operator": "ONE_OF", "value": ["on"]}),
        json!({"flagEvaluation": null, "attribute": "country", "operator": "ONE_OF", "value": ["FR"]}),
        json!({"flagEvaluation": null, "operator": "ONE_OF", "value": ["on"]}),
        json!({"flagEvaluation": {}, "operator": "ONE_OF", "value": ["on"]}),
        json!({"flagEvaluation": {"key": ""}, "operator": "ONE_OF", "value": ["on"]}),
        json!({"flagEvaluation": {"key": "parent"}, "operator": "IS_NULL", "value": false}),
        json!({"flagEvaluation": {"key": "parent"}, "operator": "ONE_OF", "value": "on"}),
        json!({"flagEvaluation": {"key": "parent"}, "operator": "ONE_OF", "value": [true]}),
        json!({"attribute": null, "operator": "ONE_OF", "value": ["on"]}),
    ] {
        let w = wire(json!({"root": flag(vec![invalid]), "parent": flag(vec![])}));
        assert_eq!(
            eval(w.clone()).result.unwrap_err(),
            EvaluationError::FlagConfigurationInvalid
        );
        let mut dependent = w.clone();
        dependent["flags"]["bad"] = dependent["flags"]["root"].clone();
        dependent["flags"]["root"] = flag(vec![dependency("bad")]);
        let result = eval(dependent);
        assert_eq!(
            cause(result.result.as_ref().unwrap_err()),
            &EvaluationError::FlagConfigurationInvalid
        );
        assert_eq!(result.evaluations.len(), 1);
        assert_eq!(result.evaluations[0].flag_key.as_str(), "bad");
        assert!(!result.evaluations[0].exposure_eligible);
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
fn an_empty_attribute_selector_without_a_reference_remains_valid() {
    let condition = json!({"attribute": "", "operator": "IS_NULL", "value": true});
    let result = eval(wire(json!({"root": flag(vec![condition])})));
    assert_eq!(result.result.unwrap().variation_key.as_str(), "on");
    assert!(result.evaluations.is_empty());
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
fn invalid_depth_limits_only_fail_when_a_dependency_executes() {
    for limit in [
        json!(-1),
        json!(256),
        json!(1.5),
        json!("10"),
        json!(true),
        json!(false),
        json!(null),
        json!([]),
        json!({}),
    ] {
        let mut w = wire(json!({"root": flag(vec![]),
            "dependent": flag(vec![dependency("parent")]), "parent": flag(vec![])}));
        w["evaluatorParams"] = json!({"maxDependencyDepth": limit});
        let bytes = serde_json::to_vec(&w).unwrap();
        let parsed = UniversalFlagConfig::from_json(bytes.clone()).unwrap();
        assert_eq!(parsed.to_json(), bytes);
        let config = Configuration::from_server_response(parsed);
        let context = EvaluationContext::new(Some("subject".into()), Default::default());
        let now = "2026-10-28T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        assert_eq!(
            config
                .eval_flag("root", &context, ExpectedFlagType::Boolean, now)
                .unwrap()
                .variation_key
                .as_str(),
            "on",
            "limit={limit}"
        );
        let result = config.eval_flag_with_trace(
            "dependent",
            &context,
            ExpectedFlagType::Boolean,
            now,
            EvaluationOptions::default(),
        );
        assert_eq!(
            result.result.unwrap_err(),
            EvaluationError::ConfigurationParseError,
            "limit={limit}"
        );
        assert!(result.evaluations.is_empty());
    }
}

#[test]
fn invalid_depth_limits_do_not_affect_skipped_dependency_conditions() {
    let mut future = flag(vec![dependency("missing")]);
    future["allocations"][0]["startAt"] = json!("2026-10-29T00:00:00Z");
    let mut expired = flag(vec![dependency("missing")]);
    expired["allocations"][0]["endAt"] = json!("2026-10-27T00:00:00Z");
    let mut earlier_allocation = flag(vec![dependency("missing")]);
    earlier_allocation["allocations"]
        .as_array_mut()
        .unwrap()
        .insert(0, allocation(vec![], "on"));
    let mut earlier_rule = flag(vec![dependency("missing")]);
    earlier_rule["allocations"][0]["rules"]
        .as_array_mut()
        .unwrap()
        .insert(0, json!({"conditions": []}));
    for (name, root, expected) in [
        (
            "short-circuit",
            flag(vec![fails(), dependency("missing")]),
            "off",
        ),
        ("future allocation", future, "off"),
        ("expired allocation", expired, "off"),
        ("earlier allocation", earlier_allocation, "on"),
        ("earlier rule", earlier_rule, "on"),
    ] {
        for limit in [json!(-1), json!(null)] {
            let mut w = wire(json!({"root": root}));
            w["evaluatorParams"] = json!({"maxDependencyDepth": limit});
            let result = eval(w);
            assert_eq!(
                result.result.unwrap().variation_key.as_str(),
                expected,
                "{name} limit={limit}"
            );
            assert!(result.evaluations.is_empty());
        }
    }
}

#[test]
fn experimentation_opt_out_can_skip_an_invalid_dependency_limit() {
    let mut root = flag(vec![dependency("missing")]);
    root["allocations"][1]["doLog"] = json!(false);
    let mut w = wire(json!({"root": root}));
    w["evaluatorParams"] = json!({"maxDependencyDepth": null});
    let result = evaluate(
        w,
        EvaluationOptions {
            experimentation_opt_out_mode: ExperimentationOptOutMode::Immediate,
        },
        EvaluationContext::new(
            Some("subject".into()),
            Arc::new(HashMap::from([(
                "dd_experimentation_opt_out_at".into(),
                "2026-10-26T00:00:00Z".into(),
            )])),
        ),
    );
    assert_eq!(result.result.unwrap().variation_key.as_str(), "off");
    assert!(result.evaluations.is_empty());
}

#[test]
fn omitted_depth_limits_default_to_ten() {
    for params in [
        json!(null),
        json!({}),
        json!({"other": true}),
        json!(false),
        json!([]),
        json!("ignored"),
    ] {
        for edges in [10, 11] {
            let mut w = chain(edges);
            w["evaluatorParams"] = params.clone();
            let result = eval(w);
            assert_eq!(
                result.result.is_ok(),
                edges == 10,
                "params={params} edges={edges}"
            );
            if edges == 11 {
                assert_eq!(
                    cause(result.result.as_ref().unwrap_err()),
                    &EvaluationError::DependencyDepthExceeded
                );
            }
        }
    }
}

#[test]
fn integral_json_numbers_are_valid_depth_limits() {
    for (limit, edges) in [
        (json!(0), 0),
        (json!(0.0), 0),
        (json!(-0.0), 0),
        (json!(1.0), 1),
        (json!(255), 255),
        (json!(255.0), 255),
    ] {
        let mut w = chain(edges);
        w["evaluatorParams"] = json!({"maxDependencyDepth": limit});
        let bytes = serde_json::to_vec(&w).unwrap();
        assert_eq!(
            UniversalFlagConfig::from_json(bytes.clone())
                .unwrap()
                .to_json(),
            bytes
        );
        assert!(eval(w).result.is_ok(), "limit={limit}");
    }
}

#[test]
fn depth_rejected_prerequisites_are_recorded_before_their_ancestors() {
    for (edges, limit, expected) in [(1, 0, vec!["f1"]), (3, 2, vec!["f3", "f2", "f1"])] {
        let mut w = chain(edges);
        w["evaluatorParams"] = json!({"maxDependencyDepth": limit});
        let result = eval(w);
        assert_eq!(
            cause(result.result.as_ref().unwrap_err()),
            &EvaluationError::DependencyDepthExceeded
        );
        assert_eq!(
            result
                .evaluations
                .iter()
                .map(|record| record.flag_key.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        for record in &result.evaluations {
            assert_eq!(
                cause(record.result.as_ref().unwrap_err()),
                &EvaluationError::DependencyDepthExceeded
            );
            assert!(!record.exposure_eligible);
        }
    }
}

#[test]
fn depth_rejection_does_not_duplicate_active_or_memoized_nodes_or_include_the_root() {
    for (flags, limit, expected) in [
        (json!({"root": flag(vec![dependency("root")])}), 0, vec![]),
        (
            json!({"root": flag(vec![dependency("a")]), "a": flag(vec![dependency("a")])}),
            1,
            vec!["a"],
        ),
        (
            json!({"root": flag(vec![dependency("a"), dependency("b")]),
            "a": flag(vec![]), "b": flag(vec![dependency("a")])}),
            1,
            vec!["a", "b"],
        ),
    ] {
        let mut w = wire(flags);
        w["evaluatorParams"] = json!({"maxDependencyDepth": limit});
        let result = eval(w);
        assert_eq!(
            cause(result.result.as_ref().unwrap_err()),
            &EvaluationError::DependencyDepthExceeded
        );
        assert_eq!(
            result
                .evaluations
                .iter()
                .map(|record| record.flag_key.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(
            result
                .evaluations
                .iter()
                .all(|record| !record.exposure_eligible)
        );
    }
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

#[test]
fn memoized_transitive_prerequisite_keeps_nested_exposures_when_reused() {
    let mut root = flag(vec![]);
    root["allocations"][0]["rules"] = json!([
        {"conditions": [dependency("parent"), fails()]},
        {"conditions": [dependency("parent")]}
    ]);
    let r = eval(wire(
        json!({"root": root, "parent": flag(vec![dependency("leaf")]), "leaf": flag(vec![])}),
    ));
    assert_eq!(r.result.unwrap().variation_key.as_str(), "on");
    assert_eq!(
        r.evaluations
            .iter()
            .map(|e| e.flag_key.as_str())
            .collect::<Vec<_>>(),
        ["leaf", "parent"]
    );
    assert!(r.evaluations.iter().all(|e| e.exposure_eligible));
}

#[test]
fn transitive_success_then_root_rule_rejection_exposes_nothing() {
    let r = eval(wire(
        json!({"root": flag(vec![dependency("parent"), fails()]),
        "parent": flag(vec![dependency("leaf")]), "leaf": flag(vec![])}),
    ));
    assert_eq!(r.result.unwrap().variation_key.as_str(), "off");
    assert_eq!(r.evaluations.len(), 2);
    assert!(
        r.evaluations
            .iter()
            .all(|e| e.result.is_ok() && !e.exposure_eligible)
    );
}

#[test]
fn prerequisite_first_reached_in_rejected_allocation_is_exposed_once_by_later_allocation() {
    let mut root = flag(vec![dependency("parent"), fails()]);
    root["allocations"]
        .as_array_mut()
        .unwrap()
        .insert(1, allocation(vec![dependency("parent")], "on"));
    let r = eval(wire(json!({"root": root, "parent": flag(vec![])})));
    assert_eq!(r.result.unwrap().variation_key.as_str(), "on");
    assert_eq!(r.evaluations.len(), 1);
    assert!(r.evaluations[0].exposure_eligible);
}
