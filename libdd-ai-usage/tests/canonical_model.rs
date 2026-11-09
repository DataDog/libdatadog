// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The model's canonical name on cost points
//! (PORTABLE-METRICS.md#canonical-model). The expected values come from that
//! text, not from the projectors. Every rejection is compared by code.

use std::collections::BTreeMap;

use libdd_ai_usage::{Json, Profile, Projection, project_result};

const COST_COUNTER: &str = "trajectory.gen_ai.client.inference.usage.cost";
const COST_HISTOGRAM: &str = "trajectory.gen_ai.client.operation.cost";
const CANONICAL: [&str; 4] = [
    "trajectory.canonical.provider",
    "trajectory.canonical.model",
    "trajectory.canonical.family",
    "trajectory.cost.priced_as",
];

const USAGE: &str = r#""observation_point": "gateway", "operation_name": "chat", "provider_name": "examplegateway", "request_model": "ex-large-latest", "duration_seconds": 0.9, "streaming": false, "input_tokens": 1000, "output_tokens": 500"#;
const COST: &str = r#", "cost_usd": 0.0105, "cost_source": "estimated""#;
const NAME: &str = r#", "canonical_provider": "exampleai", "canonical_model": "ex-large-1", "model_family": "ex-large""#;

fn project(members: &str) -> Result<Projection, &'static str> {
    let input = format!("{{{USAGE}{members}}}");
    let input = Json::parse(&input).unwrap_or_else(|e| panic!("{input}: {e}"));
    project_result(Profile::ProviderAttempt, &input).map_err(|error| error.code().as_str())
}

/// Per metric name, the canonical attributes its points carry.
fn canonical(projection: &Projection) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for point in &projection.points {
        let found: BTreeMap<String, String> = CANONICAL
            .iter()
            .filter_map(|key| {
                point
                    .attributes
                    .get(*key)
                    .map(|value| (key.to_string(), value.clone()))
            })
            .collect();
        if !found.is_empty() {
            out.insert(point.name.clone(), found);
        }
    }
    out
}

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

#[test]
fn only_cost_points_carry_the_canonical_name() {
    let projection = project(&format!(
        r#"{COST}{NAME}, "cost_priced_as_model": "ex-large-0""#
    ))
    .expect("valid");
    let name = [
        ("trajectory.canonical.provider", "exampleai"),
        ("trajectory.canonical.model", "ex-large-1"),
        ("trajectory.canonical.family", "ex-large"),
    ];
    let mut counter = name.to_vec();
    counter.push(("trajectory.cost.priced_as", "ex-large-0"));
    // Both cost points carry the name; the priced-as model is on the Counter
    // only.
    let mut want = BTreeMap::new();
    want.insert(COST_COUNTER.to_string(), map(&counter));
    want.insert(COST_HISTOGRAM.to_string(), map(&name));
    assert_eq!(canonical(&projection), want);
    // The names as received are never rewritten, on any point.
    for point in &projection.points {
        assert_eq!(
            point.attributes["gen_ai.provider.name"], "examplegateway",
            "{}",
            point.name
        );
        assert_eq!(
            point.attributes["gen_ai.request.model"], "ex-large-latest",
            "{}",
            point.name
        );
    }
}

#[test]
fn a_canonical_name_is_never_filled_in() {
    assert!(canonical(&project(COST).expect("valid")).is_empty());
    let nulls = format!(
        r#"{COST}, "canonical_provider": null, "canonical_model": null, "model_family": null, "cost_priced_as_model": null"#
    );
    assert!(canonical(&project(&nulls).expect("valid")).is_empty());
}

#[test]
fn a_canonical_name_without_a_cost_is_valid_and_unrecorded() {
    let projection = project(NAME).expect("valid");
    assert!(canonical(&projection).is_empty());
    assert!(
        projection
            .points
            .iter()
            .all(|point| point.name != COST_COUNTER && point.name != COST_HISTOGRAM)
    );
}

#[test]
fn canonical_name_rejections() {
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "a model of the wrong type",
            format!(r#"{COST}, "canonical_model": 5"#),
            "field_type_invalid",
        ),
        (
            "a provider outside the grammar",
            format!(
                r#"{COST}, "canonical_provider": "Example AI", "canonical_model": "ex-large-1""#
            ),
            "identifier_invalid",
        ),
        (
            "an empty provider",
            format!(r#"{COST}, "canonical_provider": "", "canonical_model": "ex-large-1""#),
            "identifier_invalid",
        ),
        (
            "an empty model",
            format!(r#"{COST}, "canonical_model": """#),
            "value_not_allowed",
        ),
        (
            "an empty family",
            format!(r#"{COST}, "canonical_model": "ex-large-1", "model_family": """#),
            "value_not_allowed",
        ),
        (
            "an empty priced-as model",
            format!(r#"{COST}, "canonical_model": "ex-large-1", "cost_priced_as_model": """#),
            "value_not_allowed",
        ),
        (
            "a provider without the model",
            format!(r#"{COST}, "canonical_provider": "exampleai""#),
            "fields_contradict",
        ),
        (
            "a family without the model",
            format!(r#"{COST}, "model_family": "ex-large""#),
            "fields_contradict",
        ),
        (
            "a priced-as model without the model",
            format!(r#"{COST}, "cost_priced_as_model": "ex-large-0""#),
            "fields_contradict",
        ),
        (
            "a priced-as model on a calculated cost",
            format!(
                r#", "cost_usd": 0.0105, "cost_source": "calculated"{NAME}, "cost_priced_as_model": "ex-large-0""#
            ),
            "fields_contradict",
        ),
        (
            "a priced-as model on a reported cost",
            format!(
                r#", "cost_usd": 0.0105, "cost_source": "reported"{NAME}, "cost_priced_as_model": "ex-large-0""#
            ),
            "fields_contradict",
        ),
        (
            "a priced-as model with no cost",
            format!(r#"{NAME}, "cost_priced_as_model": "ex-large-0""#),
            "fields_contradict",
        ),
        // Order: the provider's grammar, an empty part, a part without the
        // model, then the cost rules.
        (
            "the provider's grammar before an empty model",
            format!(r#"{COST}, "canonical_provider": "Example AI", "canonical_model": """#),
            "identifier_invalid",
        ),
        (
            "an empty family before a family without the model",
            format!(r#"{COST}, "model_family": """#),
            "value_not_allowed",
        ),
        (
            "a cost source outside its values before the priced-as rule",
            format!(
                r#", "cost_usd": 0.0105, "cost_source": "guessed"{NAME}, "cost_priced_as_model": "ex-large-0""#
            ),
            "value_not_allowed",
        ),
    ];
    for (name, members, code) in cases {
        assert_eq!(project(&members).err(), Some(code), "{name}");
    }
}
