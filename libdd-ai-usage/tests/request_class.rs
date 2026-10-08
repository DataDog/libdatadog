//! The service tier and the batch flag on token and cost Counters
//! (PORTABLE-METRICS.md#service-tier-and-batch). Every rejection is compared
//! by code.

use libdd_ai_usage::{Instrument, Json, Profile, Projection, project_result};

const TIER: &str = "trajectory.request.service_tier";
const BATCH: &str = "trajectory.request.batch";

const ATTEMPT: &str = r#""observation_point": "gateway", "operation_name": "chat", "provider_name": "exampleai", "request_model": "ex-fast-1", "duration_seconds": 1.2, "streaming": false, "input_tokens": 1000, "output_tokens": 500, "cost_nano_usd": 11500000, "cost_source": "calculated", "cost_rate_card": "synthetic-multipliers@1""#;

const BREAKDOWN: &str = r#""observation_point": "gateway", "operation_name": "chat", "provider_name": "exampleai", "request_model": "ex-long-1", "input_tokens": 250000, "input_basis": "includes_cache", "output_tokens": 100, "cache_read_input_tokens": 200000, "cache_write_input_tokens": 0, "reasoning_output_tokens": 40"#;

fn project(profile: Profile, base: &str, members: &str) -> Result<Projection, &'static str> {
    let input = format!("{{{base}{members}}}");
    let input = Json::parse(&input).unwrap_or_else(|e| panic!("{input}: {e}"));
    project_result(profile, &input).map_err(|error| error.code().as_str())
}

fn attempt(members: &str) -> Result<Projection, &'static str> {
    project(Profile::ProviderAttempt, ATTEMPT, members)
}

fn breakdown(members: &str) -> Result<Projection, &'static str> {
    project(Profile::TokenBreakdown, BREAKDOWN, members)
}

/// The names of the points that carry `key` with `value`, and of the
/// Counters that do not carry `key` at all.
fn carriers(projection: &Projection, key: &str, value: &str) -> (Vec<String>, Vec<String>) {
    let mut with = Vec::new();
    let mut without = Vec::new();
    for point in &projection.points {
        match point.attributes.get(key) {
            Some(found) => {
                assert_eq!(found, value, "{} {key}", point.name);
                with.push(point.name.clone());
            }
            None if point.instrument == Instrument::Counter => without.push(point.name.clone()),
            None => {}
        }
    }
    with.sort();
    without.sort();
    (with, without)
}

const ATTEMPT_COUNTERS: [&str; 3] = [
    "gen_ai.client.inference.usage.input_tokens",
    "gen_ai.client.inference.usage.output_tokens",
    "trajectory.gen_ai.client.inference.usage.cost",
];

#[test]
fn the_token_and_cost_counters_carry_a_stated_tier_and_a_true_batch() {
    let projection = attempt(r#", "service_tier": "priority", "batch": true"#).unwrap();
    for (key, value) in [(TIER, "priority"), (BATCH, "true")] {
        let (with, without) = carriers(&projection, key, value);
        assert_eq!(with, ATTEMPT_COUNTERS, "{key}");
        assert!(without.is_empty(), "{key}: {without:?}");
    }
}

#[test]
fn a_histogram_never_carries_the_tier_or_the_batch_flag() {
    let projection = attempt(r#", "service_tier": "priority", "batch": true"#).unwrap();
    let histograms: Vec<_> = projection
        .points
        .iter()
        .filter(|point| point.instrument == Instrument::Histogram)
        .collect();
    assert!(!histograms.is_empty());
    for point in histograms {
        assert!(!point.attributes.contains_key(TIER), "{}", point.name);
        assert!(!point.attributes.contains_key(BATCH), "{}", point.name);
    }
}

#[test]
fn an_unstated_tier_and_a_batch_that_is_not_true_add_nothing() {
    let plain = attempt("").unwrap();
    for members in [
        r#", "batch": false"#,
        r#", "service_tier": null, "batch": null"#,
        r#", "service_tier": null, "batch": false"#,
    ] {
        let projection = attempt(members).unwrap();
        assert_eq!(projection.points, plain.points, "{members}");
    }
    for point in &plain.points {
        assert!(!point.attributes.contains_key(TIER), "{}", point.name);
        assert!(!point.attributes.contains_key(BATCH), "{}", point.name);
    }
}

#[test]
fn the_tier_and_the_batch_flag_are_independent_of_each_other_and_of_the_band() {
    let tier = attempt(r#", "service_tier": "flex""#).unwrap();
    assert_eq!(carriers(&tier, TIER, "flex").0, ATTEMPT_COUNTERS);
    assert!(carriers(&tier, BATCH, "true").0.is_empty());

    let batch = attempt(r#", "batch": true"#).unwrap();
    assert_eq!(carriers(&batch, BATCH, "true").0, ATTEMPT_COUNTERS);
    assert!(carriers(&batch, TIER, "flex").0.is_empty());

    let all =
        attempt(r#", "service_tier": "flex", "batch": true, "context_band": "over-200k""#).unwrap();
    for (key, value) in [
        (TIER, "flex"),
        (BATCH, "true"),
        ("trajectory.context.band", "over-200k"),
    ] {
        assert_eq!(carriers(&all, key, value).0, ATTEMPT_COUNTERS, "{key}");
    }
}

#[test]
fn every_breakdown_counter_carries_the_tier_and_the_batch_flag() {
    let projection = breakdown(r#", "service_tier": "priority", "batch": true"#).unwrap();
    assert!(!projection.points.is_empty());
    for (key, value) in [(TIER, "priority"), (BATCH, "true")] {
        let (with, without) = carriers(&projection, key, value);
        assert_eq!(with.len(), projection.points.len(), "{key}");
        assert!(without.is_empty(), "{key}: {without:?}");
    }
    let plain = breakdown("").unwrap();
    let unstated = breakdown(r#", "service_tier": null, "batch": false"#).unwrap();
    assert_eq!(unstated.points, plain.points);
}

#[test]
fn a_tier_is_an_identifier_and_a_batch_is_a_boolean() {
    let cases = [
        (r#", "service_tier": 1"#, "field_type_invalid"),
        (r#", "service_tier": true"#, "field_type_invalid"),
        (r#", "service_tier": ["priority"]"#, "field_type_invalid"),
        (r#", "batch": "true""#, "field_type_invalid"),
        (r#", "batch": 1"#, "field_type_invalid"),
        (r#", "service_tier": """#, "identifier_invalid"),
        (r#", "service_tier": "on demand""#, "identifier_invalid"),
        (r#", "service_tier": "priority\n""#, "identifier_invalid"),
        (
            r#", "service_tier": "abcdefghijklmnopqrstuvwxyz0123456""#,
            "identifier_invalid",
        ),
    ];
    for (members, expected) in cases {
        assert_eq!(attempt(members).err(), Some(expected), "attempt {members}");
        assert_eq!(
            breakdown(members).err(),
            Some(expected),
            "breakdown {members}"
        );
    }
    let longest = r#", "service_tier": "abcdefghijklmnopqrstuvwxyz012345""#;
    assert!(attempt(longest).is_ok());
}

#[test]
fn the_tier_is_checked_after_every_type_and_before_the_cost() {
    assert_eq!(
        attempt(r#", "service_tier": "on demand", "batch": "yes""#).err(),
        Some("field_type_invalid")
    );
    // A cost with no source is a contradiction, checked after the tier.
    let no_source = r#""observation_point": "gateway", "operation_name": "chat", "provider_name": "exampleai", "duration_seconds": 1, "streaming": false, "cost_nano_usd": 1"#;
    assert_eq!(
        project(Profile::ProviderAttempt, no_source, "").err(),
        Some("fields_contradict")
    );
    assert_eq!(
        project(
            Profile::ProviderAttempt,
            no_source,
            r#", "service_tier": "on demand""#
        )
        .err(),
        Some("identifier_invalid")
    );
}
