//! Rules of the contract that the shared cases do not pin, or pin once:
//! the order of checks and partial usage. Every rejection is compared by
//! code.

use libdd_ai_usage::{
    IssueCode, Json, MetricPoint, Profile, Projection, ReportedUsage, normalize_usage,
    project_result,
};

fn project(profile: Profile, input: &str) -> Result<Projection, &'static str> {
    let input = with_observation_point(profile, input);
    let input = Json::parse(&input).unwrap_or_else(|e| panic!("{input}: {e}"));
    project_result(profile, &input).map_err(|error| error.code().as_str())
}

/// Every input states where it was observed (PORTABLE-METRICS.md#observation-point).
/// The rules here are not about that, so an input object that does not
/// state it gets one.
fn with_observation_point(profile: Profile, input: &str) -> String {
    let point = match profile {
        Profile::GatewayRequest => return input.to_string(),
        _ => "gateway",
    };
    let trimmed = input.trim_start();
    if !trimmed.starts_with('{') || input.contains("\"observation_point\"") {
        return input.to_string();
    }
    let rest = trimmed[1..].trim_start();
    let separator = if rest.starts_with('}') { "" } else { ", " };
    format!(r#"{{"observation_point": "{point}"{separator}{rest}"#)
}

fn attempt(members: &str) -> Result<Projection, &'static str> {
    project(Profile::ProviderAttempt, &format!("{{{members}}}"))
}

const BASE: &str = r#""operation_name": "chat", "provider_name": "exampleai", "duration_seconds": 1, "streaming": false"#;

fn code(members: &str) -> &'static str {
    match attempt(members) {
        Ok(_) => "accepted",
        Err(code) => code,
    }
}

fn issues(projection: &Projection) -> Vec<&'static str> {
    projection
        .issues
        .iter()
        .map(|issue| issue.as_str())
        .collect()
}

fn value_of(projection: &Projection, name: &str) -> Vec<f64> {
    let named = |p: &&MetricPoint| p.name == name;
    projection
        .points
        .iter()
        .filter(named)
        .map(|p| p.value)
        .collect()
}

#[test]
fn provider_attempt_checks_run_in_the_contract_order() {
    // Each input breaks the rule of one step and of a later step; the
    // earlier step's code is reported.
    let steps: [(&str, &str); 14] = [
        (
            r#""operation_name": 5, "input_tokens": -1"#,
            "field_type_invalid",
        ),
        (
            r#""operation_name": "chat", "output_tokens": -1, "input_tokens": 1.5, "input_basis": "x""#,
            "count_invalid",
        ),
        (
            r#""input_basis": "x", "input_tokens_by_modality": {"video": 1}"#,
            "value_not_allowed",
        ),
        (
            r#""provider_name": "Bad Name", "input_tokens_by_modality": {"video": 1}"#,
            "value_not_allowed",
        ),
        (r#""provider_name": "Bad Name""#, "required_field_missing"),
        (
            r#""operation_name": "summarize", "provider_name": "Bad Name""#,
            "value_not_allowed",
        ),
        (
            r#""operation_name": "chat", "provider_name": "Bad Name", "duration_seconds": -1"#,
            "identifier_invalid",
        ),
        (
            r#""operation_name": "chat", "provider_name": "exampleai", "duration_seconds": -1"#,
            "duration_invalid",
        ),
        (
            r#""operation_name": "chat", "provider_name": "exampleai", "duration_seconds": 1, "time_to_first_chunk_seconds": -1"#,
            "required_field_missing",
        ),
        (
            r#""operation_name": "chat", "provider_name": "exampleai", "duration_seconds": 1, "streaming": false, "time_to_first_chunk_seconds": -1, "context_band": "a b""#,
            "duration_invalid",
        ),
        (
            r#""operation_name": "chat", "provider_name": "exampleai", "duration_seconds": 1, "streaming": false, "time_to_first_chunk_seconds": 1, "context_band": "a b""#,
            "fields_contradict",
        ),
        (
            r#""operation_name": "chat", "provider_name": "exampleai", "duration_seconds": 1, "streaming": false, "context_band": "a b", "cost_usd": 1"#,
            "identifier_invalid",
        ),
        (
            r#""operation_name": "chat", "provider_name": "exampleai", "duration_seconds": 1, "streaming": false, "cost_usd": 1, "error_type": "Bad Type""#,
            "fields_contradict",
        ),
        (
            r#""operation_name": "chat", "provider_name": "exampleai", "duration_seconds": 1, "streaming": false, "error_type": "Bad Type""#,
            "identifier_invalid",
        ),
    ];
    for (members, expected) in steps {
        assert_eq!(code(members), expected, "{members}");
    }
}

#[test]
fn count_checks_follow_the_field_order_and_do_not_depend_on_member_order() {
    // Both inputs hold the same two faults, written in opposite orders.
    for members in [
        r#""input_tokens": -1, "input_basis": "partial""#,
        r#""input_basis": "partial", "input_tokens": -1"#,
    ] {
        assert_eq!(code(&format!("{BASE}, {members}")), "count_invalid");
    }
    for members in [
        r#""input_tokens_by_modality": {"video": 3, "text": -1}"#,
        r#""input_tokens_by_modality": {"text": -1, "video": 3}"#,
        r#""output_tokens_by_modality": {"zzz": 3, "audio": 0.5}"#,
    ] {
        assert_eq!(code(&format!("{BASE}, {members}")), "count_invalid");
    }
    // A part under an unknown key is not count-checked; its key is the fault.
    assert_eq!(
        code(&format!(
            r#"{BASE}, "input_tokens_by_modality": {{"video": -1}}"#
        )),
        "value_not_allowed"
    );
}

/// The verdicts are read from PORTABLE-METRICS.md, "Lower bound" and
/// "Contradictory cost", not from running the projector: `cost_bound` is
/// valid on a calculated cost and on an estimated cost that states
/// `cost_rate_card`. A stated card outside the grammar is left off the point
/// and the bound is kept.
#[test]
fn a_lower_bound_needs_a_calculation_or_an_estimate_that_names_its_card() {
    const COUNTER: &str = "trajectory.gen_ai.client.inference.usage.cost";
    const HISTOGRAM: &str = "trajectory.gen_ai.client.operation.cost";
    fn named<'a>(projection: &'a Projection, name: &str) -> &'a MetricPoint {
        let mut found = projection.points.iter().filter(|p| p.name == name);
        let point = found.next().unwrap_or_else(|| panic!("no {name}"));
        assert!(found.next().is_none(), "two {name}");
        point
    }
    fn attribute<'a>(point: &'a MetricPoint, key: &str) -> Option<&'a str> {
        point.attributes.get(key).map(String::as_str)
    }
    let bounded = |members: &str| {
        attempt(&format!(
            r#"{BASE}, "cost_nano_usd": 18600000, "cost_bound": "lower", {members}"#
        ))
    };
    let kept: [(&str, Option<&str>, &[&str]); 4] = [
        (
            r#""cost_source": "estimated", "cost_rate_card": "synthetic-cache-lifetime@1""#,
            Some("synthetic-cache-lifetime@1"),
            &[],
        ),
        (
            r#""cost_source": "estimated", "cost_rate_card": "Synthetic-Cache-Lifetime@1""#,
            None,
            &["cost_rate_card_dropped"],
        ),
        (
            r#""cost_source": "calculated", "cost_rate_card": "synthetic-cache-lifetime@1""#,
            Some("synthetic-cache-lifetime@1"),
            &[],
        ),
        (r#""cost_source": "calculated""#, None, &[]),
    ];
    for (members, label, want_issues) in kept {
        let recorded = bounded(members).unwrap_or_else(|code| panic!("{members}: {code}"));
        assert_eq!(issues(&recorded), want_issues, "{members}");
        let source = if members.contains("estimated") {
            "estimated"
        } else {
            "calculated"
        };
        let total = named(&recorded, COUNTER);
        assert_eq!(total.value, 18600000.0, "{members}");
        assert_eq!(attribute(total, "trajectory.cost.source"), Some(source));
        assert_eq!(attribute(total, "trajectory.cost.bound"), Some("lower"));
        assert_eq!(attribute(total, "trajectory.cost.rate_card"), label);
        let sample = named(&recorded, HISTOGRAM);
        assert_eq!(sample.value, 0.0186, "{members}");
        assert_eq!(attribute(sample, "trajectory.cost.source"), Some(source));
        assert_eq!(attribute(sample, "trajectory.cost.bound"), None);
        assert_eq!(attribute(sample, "trajectory.cost.rate_card"), None);
    }
    for members in [
        r#""cost_source": "estimated""#,
        r#""cost_source": "estimated", "cost_rate_card": null"#,
        r#""cost_source": "reported""#,
    ] {
        assert_eq!(
            bounded(members).err(),
            Some("fields_contradict"),
            "{members}"
        );
    }
}

#[test]
fn the_cost_contradictions_run_in_their_own_order() {
    let cost = |members: &str| code(&format!("{BASE}, {members}"));
    assert_eq!(
        cost(r#""cost_usd": 1, "cost_bound": "upper""#),
        "fields_contradict"
    );
    assert_eq!(cost(r#""cost_source": "guessed""#), "fields_contradict");
    assert_eq!(
        cost(r#""cost_usd": 1, "cost_source": "guessed", "cost_bound": "upper""#),
        "value_not_allowed"
    );
    assert_eq!(
        cost(r#""cost_usd": 1, "cost_source": "reported", "cost_bound": "upper""#),
        "value_not_allowed"
    );
    assert_eq!(
        cost(
            r#""cost_usd": 1, "cost_source": "reported", "cost_bound": "lower", "cost_rate_card": "a@1""#
        ),
        "fields_contradict"
    );
    // A label with no cost at all is a label on a cost that is not calculated.
    assert_eq!(cost(r#""cost_rate_card": "a@1""#), "fields_contradict");
    assert_eq!(cost(r#""cost_bound": "lower""#), "fields_contradict");
    // Two amounts disagree only when both are usable.
    assert_eq!(
        cost(r#""cost_usd": 0.25, "cost_nano_usd": 250000001, "cost_source": "reported""#),
        "fields_contradict"
    );
    let unusable = attempt(&format!(
        r#"{BASE}, "cost_usd": 0.25, "cost_nano_usd": 1.5, "cost_source": "reported""#
    ))
    .unwrap();
    assert_eq!(issues(&unusable), ["cost_unusable"]);
    assert_eq!(unusable.points.len(), 1);
}

#[test]
fn a_bad_rate_card_is_an_issue_only_on_a_cost_that_is_recorded() {
    let projection = attempt(&format!(
        r#"{BASE}, "cost_usd": -1, "cost_source": "calculated", "cost_rate_card": "bad card""#
    ))
    .unwrap();
    assert_eq!(issues(&projection), ["cost_unusable"]);
    assert_eq!(projection.points.len(), 1);
    let recorded = attempt(&format!(
        r#"{BASE}, "cost_usd": 1, "cost_source": "calculated", "cost_rate_card": "bad card""#
    ))
    .unwrap();
    assert_eq!(issues(&recorded), ["cost_rate_card_dropped"]);
    assert_eq!(recorded.points.len(), 3);
}

#[test]
fn numbers_no_double_holds_follow_the_table_for_each_kind_of_field() {
    assert_eq!(code(&BASE.replace(": 1,", ": 1e400,")), "duration_invalid");
    assert_eq!(
        code(
            &format!(r#"{BASE}, "streaming": true, "time_to_first_chunk_seconds": 1e400"#)
                .replacen(r#""streaming": false, "#, "", 1)
        ),
        "duration_invalid"
    );
    // A negative overflow is a negative number.
    assert_eq!(
        code(&format!(r#"{BASE}, "input_tokens": -1e400"#)),
        "count_invalid"
    );
    let counts = attempt(&format!(
        r#"{BASE}, "input_tokens": 1e400, "output_tokens": 1000.0, "web_search_requests": 1e400"#
    ))
    .unwrap();
    assert_eq!(issues(&counts), ["count_out_of_range"]);
    assert_eq!(
        value_of(&counts, "gen_ai.client.inference.usage.output_tokens"),
        [1000.0]
    );
    let gateway = |members: &str| {
        project(
            Profile::GatewayRequest,
            &format!(r#"{{"operation_name": "chat", "duration_seconds": 1, {members}}}"#),
        )
        .map(|_| ())
    };
    assert_eq!(gateway(r#""retries": 1e400"#), Err("count_invalid"));
    assert_eq!(gateway(r#""retries": 1.5"#), Err("count_invalid"));
    assert_eq!(gateway(r#""retries": "1""#), Err("field_type_invalid"));
    assert_eq!(gateway(r#""retries": null"#), Ok(()));
    let streaming = project(
        Profile::ProviderStreaming,
        r#"{"operation_name": "chat", "provider_name": "exampleai", "streaming": true,
            "output_chunk_intervals_seconds": [0.1, 1e400]}"#,
    );
    assert_eq!(streaming.map(|_| ()), Err("duration_invalid"));
}

#[test]
fn output_parts_without_a_total_are_recorded_with_an_issue() {
    let projection = attempt(&format!(
        r#"{BASE}, "output_tokens": 5, "reasoning_output_tokens": 2,
           "output_tokens_by_modality": {{"audio": 3, "text": 1}}"#
    ))
    .unwrap();
    assert_eq!(
        issues(&projection),
        ["output_basis_missing", "output_total_missing"]
    );
    assert!(
        value_of(
            &projection,
            "gen_ai.client.inference.operation.output_tokens"
        )
        .is_empty()
    );
    let parts: Vec<(&str, f64)> = projection
        .points
        .iter()
        .filter(|p| p.name == "gen_ai.client.inference.usage.output_tokens")
        .map(|p| (p.attributes["gen_ai.token.modality"].as_str(), p.value))
        .collect();
    assert_eq!(parts, [("text", 1.0), ("audio", 3.0)]);
}

#[test]
fn normalization_keeps_every_issue_and_each_profile_reports_its_own() {
    let usage = ReportedUsage {
        input_tokens: Some(100.0),
        output_tokens: Some(5.0),
        cache_write_input_tokens: Some(20.0),
        input_basis: Some("includes_cache".into()),
        ..ReportedUsage::default()
    };
    let normalized = normalize_usage(&usage, false).unwrap();
    assert!(normalized.issues.contains(&IssueCode::CacheReadMissing));
    assert!(
        normalized
            .issues
            .contains(&IssueCode::CacheWriteLifetimeUnknown)
    );
    assert_eq!(normalized.cache_write_unspecified, Some(20));
    assert_eq!(normalized.uncached_input, None);

    let members = r#""input_tokens": 100, "output_tokens": 5, "cache_write_input_tokens": 20, "input_basis": "includes_cache""#;
    // The provider-attempt profile records neither part, so it reports neither issue.
    let attempt = attempt(&format!("{BASE}, {members}")).unwrap();
    assert!(attempt.issues.is_empty(), "{:?}", attempt.issues);
    let breakdown = project(
        Profile::TokenBreakdown,
        &format!(r#"{{"operation_name": "chat", "provider_name": "exampleai", {members}}}"#),
    )
    .unwrap();
    assert_eq!(
        issues(&breakdown),
        ["cache_read_missing", "cache_write_lifetime_unknown"]
    );
}

#[test]
fn an_inclusive_total_above_the_bound_is_not_known() {
    let projection = attempt(&format!(
        r#"{BASE}, "input_basis": "excludes_cache", "input_tokens": 9007199254740991,
           "cache_read_input_tokens": 1, "cache_write_input_tokens": 0"#
    ))
    .unwrap();
    assert_eq!(issues(&projection), ["count_out_of_range"]);
    assert_eq!(projection.points.len(), 1);
    let exact = attempt(&format!(
        r#"{BASE}, "input_basis": "excludes_cache", "input_tokens": 9007199254740990,
           "cache_read_input_tokens": 1, "cache_write_input_tokens": 0"#
    ))
    .unwrap();
    assert_eq!(
        value_of(&exact, "gen_ai.client.inference.operation.input_tokens"),
        [9_007_199_254_740_991.0]
    );
}

#[test]
fn a_null_modality_part_is_absent_whatever_its_key() {
    let projection = attempt(&format!(
        r#"{BASE}, "input_tokens": 100, "input_tokens_by_modality": {{"video": null, "audio": 80}}"#
    ))
    .unwrap();
    assert!(projection.issues.is_empty());
    assert_eq!(
        value_of(&projection, "gen_ai.client.inference.usage.input_tokens"),
        [80.0, 20.0]
    );
}

#[test]
fn a_missing_observation_is_required_and_another_type_is_wrong() {
    for profile in [
        Profile::ProviderAttempt,
        Profile::ProviderStreaming,
        Profile::TokenBreakdown,
        Profile::GatewayRequest,
    ] {
        assert_eq!(
            project(profile, "null").map(|_| ()),
            Err("required_field_missing")
        );
        assert_eq!(
            project(profile, "[]").map(|_| ()),
            Err("field_type_invalid")
        );
        assert_eq!(
            project(profile, "\"x\"").map(|_| ()),
            Err("field_type_invalid")
        );
    }
    assert_eq!(code(&BASE.replace("\"chat\"", "\"\"")), "value_not_allowed");
    assert_eq!(
        code(&BASE.replace("\"exampleai\"", "\"\"")),
        "identifier_invalid"
    );
    assert_eq!(
        code(&BASE.replace("\"exampleai\"", "\"exampleai\\n\"")),
        "identifier_invalid"
    );
}
