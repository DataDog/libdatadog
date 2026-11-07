// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! What a gateway derives before it can state an observation, done once
//! here: the operation from a route or path, the `error.type` of a call that
//! got no response, and a cost from per-million prices.

use crate::error_type::classify_error;
use crate::number::MAX_SAFE_INTEGER;

/// The `gen_ai.operation.name` of a gateway route or request path: Kong's AI
/// route types (`llm/v1/chat`, ...), an upstream path (`/v1/chat/completions`,
/// `/v1/messages`, `/v1/responses`, `/v1/embeddings`, `/v1/completions`,
/// Gemini's `:generateContent`, Bedrock's `/converse`), or an operation name
/// already. A query string is ignored. `None` for anything else, which is
/// not an operation these profiles measure.
pub fn operation_name_for(route_or_path: &str) -> Option<&'static str> {
    let path = route_or_path.trim();
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let path = path.trim_end_matches('/');
    match path {
        "chat" | "llm/v1/chat" | "llm/v1/responses" => return Some("chat"),
        "text_completion" | "llm/v1/completions" => return Some("text_completion"),
        "embeddings" | "llm/v1/embeddings" => return Some("embeddings"),
        _ => {}
    }
    let lower = path.to_ascii_lowercase();
    let ends = |suffix: &str| lower.ends_with(suffix);
    if ends("/embeddings") || ends(":embedcontent") || ends(":batchembedcontents") || ends("/embed")
    {
        Some("embeddings")
    } else if ends("/chat/completions")
        || ends("/messages")
        || ends("/responses")
        || ends(":generatecontent")
        || ends(":streamgeneratecontent")
        || ends("/converse")
        || ends("/converse-stream")
    {
        Some("chat")
    } else if ends("/completions") {
        Some("text_completion")
    } else {
        None
    }
}

/// How a call to a provider failed, as a gateway sees it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Failure<'a> {
    /// The HTTP status the gateway got or returned, if any.
    pub status: Option<u16>,
    /// No response came from the provider: the connection failed, was
    /// refused or reset, or timed out.
    pub no_response: bool,
    /// The call timed out (with or without a response).
    pub timed_out: bool,
    /// The SDK exception class, or the error type or code the provider
    /// reported, if any.
    pub name: Option<&'a str>,
}

/// The `error.type` of a failed call, or `None` when it did not fail. A call
/// with no response is `timeout_error` when it timed out (or the gateway
/// answered 504) and `api_connection_error` otherwise. A response is
/// classified by its name and status as [`classify_error`] does, and a
/// status other than 200 that is not a failure (a 1xx, 3xx or other 2xx)
/// is `api_error`: the provider did not answer as the API defines.
pub fn error_type_for_failure(failure: &Failure<'_>) -> Option<&'static str> {
    if failure.no_response {
        return if failure.timed_out || failure.status == Some(504) {
            classify_error(None, Some(504))
        } else {
            classify_error(Some("APIConnectionError"), None)
        };
    }
    if failure.timed_out {
        return classify_error(None, Some(504));
    }
    let name = failure.name.filter(|name| !name.trim().is_empty());
    match (name, failure.status) {
        (None, None) => None,
        (None, Some(200)) => None,
        (name, Some(status)) => classify_error(name, Some(status))
            .or_else(|| (status != 200).then_some(crate::ERROR_TYPE_API)),
        (name, None) => classify_error(name, None),
    }
}

/// Prices in USD per million tokens, as gateways configure them.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PricesPerMillion {
    pub input: f64,
    pub output: f64,
    /// Cache reads, when priced apart from input; else at `input`.
    pub cache_read: Option<f64>,
    /// Cache writes, when priced apart from input; else at `input`.
    pub cache_write: Option<f64>,
}

/// The tokens a cost is computed from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BillableTokens {
    /// All input, cache reads and writes included.
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// All output, reasoning included.
    pub output: u64,
}

/// The cost of `tokens` at `prices`, in nano-USD, rounded to the nearest:
/// uncached input at the input price, cache reads and writes at theirs (or
/// the input price), output at the output price. `None` when a price is not
/// a finite non-negative number or the cost does not fit.
pub fn estimated_cost_nano_usd(prices: &PricesPerMillion, tokens: &BillableTokens) -> Option<u64> {
    let price = |p: f64| (p.is_finite() && p >= 0.0).then_some(p);
    let input = price(prices.input)?;
    let output = price(prices.output)?;
    let cache_read = match prices.cache_read {
        Some(p) => price(p)?,
        None => input,
    };
    let cache_write = match prices.cache_write {
        Some(p) => price(p)?,
        None => input,
    };
    let uncached = tokens
        .input
        .saturating_sub(tokens.cache_read)
        .saturating_sub(tokens.cache_write);
    // USD per million tokens is 1000 nano-USD per token.
    let as_f64 = |n: u64| n as f64;
    let nanos = 1000.0
        * (as_f64(uncached) * input
            + as_f64(tokens.cache_read) * cache_read
            + as_f64(tokens.cache_write) * cache_write
            + as_f64(tokens.output) * output);
    (nanos.is_finite() && nanos <= MAX_SAFE_INTEGER as f64).then(|| nanos.round() as u64)
}
