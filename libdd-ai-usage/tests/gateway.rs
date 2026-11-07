// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! What gateways derive before they state an observation: the operation of
//! a route, the error type of a call with no response, and a cost from
//! per-million prices.

use libdd_ai_usage::{
    BillableTokens, Failure, PricesPerMillion, error_type_for_failure, estimated_cost_nano_usd,
    operation_name_for,
};

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
