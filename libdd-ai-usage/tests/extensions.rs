// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The observation extensions: what gateways derive before they state an
//! observation.

use libdd_ai_usage::{
    BillableTokens, ErrorCode, Failure, Json, PricesPerMillion, Profile, error_type_for_failure,
    estimated_cost_nano_usd, expand_observation, operation_name_for, project_extended,
};

fn json(text: &str) -> Json {
    Json::parse(text).unwrap()
}

fn expanded(profile: Profile, text: &str) -> Json {
    expand_observation(profile, &json(text))
        .unwrap()
        .into_owned()
}

#[test]
fn operations_from_routes_and_paths() {
    for (route, operation) in [
        ("llm/v1/chat", Some("chat")),
        ("llm/v1/responses", Some("chat")),
        ("llm/v1/completions", Some("text_completion")),
        ("llm/v1/embeddings", Some("embeddings")),
        ("chat", Some("chat")),
        ("/v1/chat/completions?api-version=2024-10-21", Some("chat")),
        ("/v1/messages", Some("chat")),
        ("/openai/v1/responses/", Some("chat")),
        ("/v1/embeddings", Some("embeddings")),
        ("/v1/completions", Some("text_completion")),
        (
            "/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse",
            Some("chat"),
        ),
        (
            "/v1/models/text-embedding-004:embedContent",
            Some("embeddings"),
        ),
        (
            "/model/anthropic.claude-sonnet-4/converse-stream",
            Some("chat"),
        ),
        ("/v1/models", None),
        ("preserve", None),
        ("", None),
    ] {
        assert_eq!(operation_name_for(route), operation, "{route}");
    }
}

#[test]
fn failures_without_a_response() {
    let failure = |status, no_response, timed_out, name| {
        error_type_for_failure(&Failure {
            status,
            no_response,
            timed_out,
            name,
        })
    };
    // Kong: no response, 504 upstream or a timeout is a timeout.
    assert_eq!(failure(Some(504), true, false, None), Some("timeout_error"));
    assert_eq!(failure(None, true, true, None), Some("timeout_error"));
    assert_eq!(
        failure(None, true, false, None),
        Some("api_connection_error")
    );
    assert_eq!(
        failure(Some(502), true, false, None),
        Some("api_connection_error")
    );
    // A response: its status, and a non-200 that is not a failure is api_error.
    assert_eq!(
        failure(Some(429), false, false, None),
        Some("rate_limit_error")
    );
    assert_eq!(failure(Some(204), false, false, None), Some("api_error"));
    assert_eq!(failure(Some(304), false, false, None), Some("api_error"));
    assert_eq!(failure(Some(200), false, false, None), None);
    assert_eq!(failure(None, false, false, None), None);
    // An in-band failure in a 200 is named.
    assert_eq!(
        failure(Some(200), false, false, Some("overloaded_error")),
        Some("overloaded_error").and_then(|n| libdd_ai_usage::classify_error(Some(n), None))
    );
}

#[test]
fn costs_from_prices_per_million() {
    let prices = PricesPerMillion {
        input: 3.0,
        output: 15.0,
        cache_read: Some(0.3),
        cache_write: Some(3.75),
    };
    let tokens = BillableTokens {
        input: 1_000_000,
        cache_read: 400_000,
        cache_write: 100_000,
        output: 10_000,
    };
    // 0.5M at 3 + 0.4M at 0.3 + 0.1M at 3.75 + 0.01M at 15 = 2.145 USD.
    assert_eq!(
        estimated_cost_nano_usd(&prices, &tokens),
        Some(2_145_000_000)
    );
    // Without cache prices every input token is priced as input (Kong).
    let flat = PricesPerMillion {
        cache_read: None,
        cache_write: None,
        ..prices
    };
    assert_eq!(estimated_cost_nano_usd(&flat, &tokens), Some(3_150_000_000));
    let bad = PricesPerMillion {
        input: f64::NAN,
        ..prices
    };
    assert_eq!(estimated_cost_nano_usd(&bad, &tokens), None);
}

const ATTEMPT: &str = r#"{"operation_name": "chat", "provider_name": "anthropic",
    "streaming": false, "observation_point": "gateway", "duration_ns": 1500000000,
    "input_tokens": 100, "input_basis": "excludes_cache", "output_tokens": 20,
    "output_basis": "includes_reasoning", "cache_read_input_tokens": 400,
    "cache_write_input_tokens": 0,
    "prices_per_million": {"input": 3, "output": 15},
    "zero_is_unreported": true}"#;

#[test]
fn an_extended_observation_projects_as_the_one_it_stands_for() {
    let value = expanded(Profile::ProviderAttempt, ATTEMPT);
    assert_eq!(
        value.get("duration_seconds").and_then(Json::as_f64),
        Some(1.5)
    );
    assert!(value.get("duration_ns").is_none());
    // The zero cache write is not reported; the read is.
    assert!(value.get("cache_write_input_tokens").is_none());
    assert_eq!(
        value.get("cache_read_input_tokens").and_then(Json::as_f64),
        Some(400.0)
    );
    // (100 + 400) input at 3 and 20 output at 15, per million.
    assert_eq!(
        value.get("cost_nano_usd").and_then(Json::as_f64),
        Some(1_800_000.0)
    );
    assert_eq!(
        value.get("cost_source").and_then(Json::as_str),
        Some("estimated")
    );
    let projection = project_extended(Profile::ProviderAttempt, &json(ATTEMPT)).unwrap();
    let cost = projection
        .points
        .iter()
        .find(|p| p.name == "trajectory.gen_ai.client.inference.usage.cost")
        .unwrap();
    assert_eq!(cost.exact_value, Some(1_800_000));
    assert_eq!(
        cost.attributes
            .get("trajectory.cost.source")
            .map(String::as_str),
        Some("estimated")
    );

    // A stated cost wins over prices; stated seconds over nanoseconds.
    let stated = ATTEMPT.replace(
        "\"duration_ns\": 1500000000,",
        "\"duration_ns\": 1500000000, \"duration_seconds\": 2.0, \"cost_usd\": 0.5, \"cost_source\": \"reported\",",
    );
    let value = expanded(Profile::ProviderAttempt, &stated);
    assert_eq!(
        value.get("duration_seconds").and_then(Json::as_f64),
        Some(2.0)
    );
    assert!(value.get("cost_nano_usd").is_none());

    // A failure states the error type.
    let failed = ATTEMPT.replace(
        "\"zero_is_unreported\": true",
        "\"failure\": {\"no_response\": true, \"timed_out\": true}",
    );
    let value = expanded(Profile::ProviderAttempt, &failed);
    assert_eq!(
        value.get("error_type").and_then(Json::as_str),
        Some("timeout_error")
    );
}

#[test]
fn zero_totals_are_unreported_only_on_an_estimated_side() {
    let text = r#"{"input_tokens": 0, "input_basis": "includes_cache", "output_tokens": 0,
        "output_basis": "includes_reasoning", "output_token_source": "estimated",
        "reasoning_output_tokens": 0, "zero_is_unreported": true}"#;
    let value = expanded(Profile::TokenBreakdown, text);
    assert_eq!(value.get("input_tokens").and_then(Json::as_f64), Some(0.0));
    assert!(value.get("output_tokens").is_none());
    assert!(value.get("output_basis").is_none());
    assert!(value.get("reasoning_output_tokens").is_none());
    let listed = r#"{"input_tokens": 0, "input_basis": "includes_cache",
        "zero_is_unreported": ["input_tokens"]}"#;
    let value = expanded(Profile::TokenBreakdown, listed);
    assert!(value.get("input_tokens").is_none());
    let error = expand_observation(
        Profile::TokenBreakdown,
        &json(r#"{"zero_is_unreported": ["duration_seconds"]}"#),
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ValueNotAllowed);
}

#[test]
fn nanosecond_durations_and_intervals() {
    let value = expanded(
        Profile::ProviderStreaming,
        r#"{"output_chunk_intervals_ns": [1000000, 250000000]}"#,
    );
    let intervals: Vec<f64> = value
        .get("output_chunk_intervals_seconds")
        .and_then(Json::as_array)
        .unwrap()
        .iter()
        .filter_map(Json::as_f64)
        .collect();
    assert_eq!(intervals, [0.001, 0.25]);
    for bad in [
        r#"{"duration_ns": 1.5}"#,
        r#"{"duration_ns": -1}"#,
        r#"{"duration_ns": "1"}"#,
        r#"{"output_chunk_intervals_ns": 5}"#,
        r#"{"failure": {"status": 42}}"#,
        r#"{"prices_per_million": {"input": 1}}"#,
    ] {
        let error = expand_observation(Profile::ProviderAttempt, &json(bad)).unwrap_err();
        assert_eq!(error.code(), ErrorCode::FieldTypeInvalid, "{bad}");
    }
}

#[test]
fn an_observation_without_extensions_is_not_copied() {
    let plain = json(r#"{"operation_name": "chat"}"#);
    assert!(matches!(
        expand_observation(Profile::ProviderAttempt, &plain).unwrap(),
        std::borrow::Cow::Borrowed(_)
    ));
}
