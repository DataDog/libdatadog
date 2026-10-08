// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Estimated token counts (`PORTABLE-METRICS.md#token-source`).
//! The expected values come from the contract text, not from the projectors.

use libdd_ai_usage::{Json, Profile, TOKEN_SOURCES, project_result};

const ATTRIBUTE: &str = "trajectory.token.source";

const ATTEMPT: &str = r#""operation_name": "chat", "provider_name": "exampleai", "duration_seconds": 0.8, "streaming": true, "time_to_first_chunk_seconds": 0.2, "input_tokens": 1000, "output_tokens": 500, "observation_point": "gateway""#;
const BREAKDOWN: &str = r#""operation_name": "chat", "provider_name": "exampleai", "input_tokens": 2000, "input_basis": "includes_cache", "output_tokens": 800, "output_basis": "includes_reasoning", "cache_read_input_tokens": 1024, "cache_write_input_tokens": 0, "reasoning_output_tokens": 300, "observation_point": "gateway""#;

const INPUT: [&str; 2] = [
    "gen_ai.client.inference.operation.input_tokens",
    "gen_ai.client.inference.usage.input_tokens",
];
const OUTPUT: [&str; 2] = [
    "gen_ai.client.inference.operation.output_tokens",
    "gen_ai.client.inference.usage.output_tokens",
];

/// The sorted names of the points that carry the attribute, or the
/// rejection code.
fn marked(profile: Profile, members: &str, stated: &str) -> Result<Vec<String>, &'static str> {
    let text = format!("{{{members}{stated}}}");
    let input = Json::parse(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
    let projection = project_result(profile, &input).map_err(|error| error.code().as_str())?;
    let mut names: Vec<String> = Vec::new();
    for point in &projection.points {
        if let Some(value) = point.attributes.get(ATTRIBUTE) {
            assert_eq!(value, "estimated", "{}", point.name);
            names.push(point.name.clone());
        }
    }
    names.sort();
    Ok(names)
}

fn sorted(names: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    names.sort();
    names
}

#[test]
fn each_estimated_side_marks_its_own_points() {
    assert_eq!(TOKEN_SOURCES, ["reported", "estimated"]);
    let both: Vec<&str> = INPUT.iter().chain(OUTPUT.iter()).copied().collect();
    for (stated, expected) in [
        ("", sorted(&[])),
        (
            r#", "input_token_source": "reported", "output_token_source": null"#,
            sorted(&[]),
        ),
        (r#", "input_token_source": "estimated""#, sorted(&INPUT)),
        (r#", "output_token_source": "estimated""#, sorted(&OUTPUT)),
        (
            r#", "input_token_source": "estimated", "output_token_source": "estimated""#,
            sorted(&both),
        ),
    ] {
        assert_eq!(
            marked(Profile::ProviderAttempt, ATTEMPT, stated),
            Ok(expected),
            "{stated}"
        );
    }
}

#[test]
fn the_breakdown_marks_parts_by_side() {
    assert_eq!(
        marked(
            Profile::TokenBreakdown,
            BREAKDOWN,
            r#", "output_token_source": "estimated""#
        ),
        Ok(sorted(&[
            "gen_ai.client.inference.usage.reasoning.output_tokens"
        ]))
    );
    assert_eq!(
        marked(
            Profile::TokenBreakdown,
            BREAKDOWN,
            r#", "input_token_source": "estimated""#
        ),
        Ok(sorted(&[
            "gen_ai.client.inference.usage.cache_read.input_tokens",
            "gen_ai.client.inference.usage.cache_write.input_tokens",
            "trajectory.gen_ai.client.inference.usage.uncached.input_tokens",
        ]))
    );
}

#[test]
fn rejections() {
    for (stated, code) in [
        (r#", "input_token_source": "guessed""#, "value_not_allowed"),
        (r#", "output_token_source": """#, "value_not_allowed"),
        (
            r#", "output_token_source": "Estimated""#,
            "value_not_allowed",
        ),
        (r#", "input_token_source": 1"#, "field_type_invalid"),
        (
            r#", "input_token_source": "estimated", "cost_usd": 0.01, "cost_source": "calculated""#,
            "fields_contradict",
        ),
        (
            r#", "output_token_source": "estimated", "cost_usd": 0.01, "cost_source": "calculated""#,
            "fields_contradict",
        ),
    ] {
        assert_eq!(
            marked(Profile::ProviderAttempt, ATTEMPT, stated),
            Err(code),
            "{stated}"
        );
    }
}

#[test]
fn a_reported_or_estimated_cost_beside_an_estimate_is_valid() {
    for source in ["reported", "estimated"] {
        let stated = format!(
            r#", "output_token_source": "estimated", "cost_usd": 0.01, "cost_source": "{source}""#
        );
        assert_eq!(
            marked(Profile::ProviderAttempt, ATTEMPT, &stated),
            Ok(sorted(&OUTPUT))
        );
    }
}
