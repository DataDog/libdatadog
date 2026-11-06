// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! What every gateway derives before it can state an observation, done once
//! here: the operation from a route or path, the `error.type` of a call that
//! got no response, a cost from per-million prices, integer nanosecond
//! durations, and zeros that mean "not reported".
//!
//! These are library extensions, not members of the profiles: an
//! observation that states them is expanded by [`expand_observation`] into
//! the members the profiles define, and is then projected as any other. An
//! observation that states none of them is projected as it is.

use std::borrow::Cow;

use crate::Profile;
use crate::error_type::classify_error;
use crate::json::{Json, JsonObject};
use crate::number::MAX_SAFE_INTEGER;
use crate::point::{ErrorCode, MetricError, reject};

/// The observation members [`expand_observation`] reads. None is a member of
/// a profile; each is removed once expanded.
pub const OBSERVATION_EXTENSIONS: [&str; 6] = [
    "duration_ns",
    "time_to_first_chunk_ns",
    "output_chunk_intervals_ns",
    "zero_is_unreported",
    "failure",
    "prices_per_million",
];

/// The counts `"zero_is_unreported": true` drops when they are zero: the
/// parts an SDK or proxy fills with zero when it does not know them. A side
/// whose `*_token_source` is `estimated` also drops a zero total.
pub const ZERO_IS_UNREPORTED_DEFAULT: [&str; 5] = [
    "cache_read_input_tokens",
    "cache_write_input_tokens",
    "cache_write_5m_input_tokens",
    "cache_write_1h_input_tokens",
    "reasoning_output_tokens",
];

/// The counts `"zero_is_unreported"` may name.
const ZERO_IS_UNREPORTED_ALLOWED: [&str; 7] = [
    "input_tokens",
    "output_tokens",
    "cache_read_input_tokens",
    "cache_write_input_tokens",
    "cache_write_5m_input_tokens",
    "cache_write_1h_input_tokens",
    "reasoning_output_tokens",
];

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
        (name, Some(status)) => classify_error(name, Some(status)).or_else(|| {
            if status != 200 {
                classify_error(Some("provider_error"), None)
            } else {
                None
            }
        }),
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

fn invalid<T>(message: impl Into<String>) -> Result<T, MetricError> {
    reject(ErrorCode::FieldTypeInvalid, message)
}

/// A whole, non-negative count no larger than `MAX_SAFE_INTEGER`.
fn count(value: Option<&Json>) -> Option<u64> {
    let n = value?.as_f64()?;
    (n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n <= MAX_SAFE_INTEGER as f64)
        .then_some(n as u64)
}

fn nanoseconds(key: &str, value: &Json) -> Result<f64, MetricError> {
    match count(Some(value)) {
        Some(ns) => Ok(ns as f64 / 1e9),
        None => invalid(format!("{key} must be a whole number of nanoseconds")),
    }
}

/// Set `seconds` from the integer nanoseconds `ns` when the observation does
/// not state the seconds itself.
fn seconds_from_ns(map: &mut JsonObject, ns: &str, seconds: &str) -> Result<(), MetricError> {
    let Some(value) = map.remove(ns) else {
        return Ok(());
    };
    if value.is_null() {
        return Ok(());
    }
    let converted = match &value {
        Json::Array(items) => Json::Array(
            items
                .iter()
                .map(|item| nanoseconds(ns, item).map(Json::Number))
                .collect::<Result<_, _>>()?,
        ),
        _ if ns.ends_with("intervals_ns") => {
            return invalid(format!("{ns} must be an array of nanoseconds"));
        }
        value => Json::Number(nanoseconds(ns, value)?),
    };
    if map.get(seconds).is_none_or(Json::is_null) {
        map.insert(seconds.to_string(), converted);
    }
    Ok(())
}

fn zeros_unreported(map: &mut JsonObject, setting: Json) -> Result<(), MetricError> {
    let names: Vec<String> = match setting {
        Json::Null | Json::Bool(false) => return Ok(()),
        Json::Bool(true) => {
            let mut names: Vec<String> = ZERO_IS_UNREPORTED_DEFAULT
                .iter()
                .map(|n| n.to_string())
                .collect();
            for (side, total) in [("input", "input_tokens"), ("output", "output_tokens")] {
                let source = format!("{side}_token_source");
                if map.get(&source).and_then(Json::as_str) == Some("estimated") {
                    names.push(total.to_string());
                }
            }
            names
        }
        Json::Array(items) => items
            .into_iter()
            .map(|item| match item {
                Json::String(name) if ZERO_IS_UNREPORTED_ALLOWED.contains(&name.as_str()) => {
                    Ok(name)
                }
                Json::String(name) => reject(
                    ErrorCode::ValueNotAllowed,
                    format!("zero_is_unreported cannot name {name}"),
                ),
                _ => invalid("zero_is_unreported must name counts"),
            })
            .collect::<Result<_, _>>()?,
        _ => return invalid("zero_is_unreported must be a boolean or an array of counts"),
    };
    for name in names {
        if map.get(&name).and_then(Json::as_f64) == Some(0.0) {
            map.remove(&name);
            // A total that is not reported has no basis.
            match name.as_str() {
                "input_tokens" => {
                    map.remove("input_basis");
                }
                "output_tokens" => {
                    map.remove("output_basis");
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn failure_error_type(map: &mut JsonObject, failure: Json) -> Result<(), MetricError> {
    let object = match failure {
        Json::Null => return Ok(()),
        Json::Object(object) => object,
        _ => return invalid("failure must be an object"),
    };
    let flag = |key: &str| -> Result<bool, MetricError> {
        match object.get(key) {
            None | Some(Json::Null) => Ok(false),
            Some(Json::Bool(flag)) => Ok(*flag),
            Some(_) => invalid(format!("failure.{key} must be a boolean")),
        }
    };
    let status = match object.get("status") {
        None | Some(Json::Null) => None,
        Some(value) => match count(Some(value)).filter(|s| (100..=599).contains(s)) {
            Some(status) => Some(status as u16),
            None => return invalid("failure.status must be an HTTP status"),
        },
    };
    let name = match object.get("name") {
        None | Some(Json::Null) => None,
        Some(Json::String(name)) => Some(name.as_str()),
        Some(_) => return invalid("failure.name must be a string"),
    };
    let derived = error_type_for_failure(&Failure {
        status,
        no_response: flag("no_response")?,
        timed_out: flag("timed_out")?,
        name,
    });
    if let Some(error_type) = derived
        && map.get("error_type").is_none_or(Json::is_null)
    {
        map.insert("error_type".into(), Json::String(error_type.into()));
    }
    Ok(())
}

fn priced_cost(map: &mut JsonObject, prices: Json) -> Result<(), MetricError> {
    let object = match prices {
        Json::Null => return Ok(()),
        Json::Object(object) => object,
        _ => return invalid("prices_per_million must be an object"),
    };
    let price = |key: &str, required: bool| -> Result<Option<f64>, MetricError> {
        match object.get(key) {
            None | Some(Json::Null) if !required => Ok(None),
            Some(Json::Number(p)) if p.is_finite() && *p >= 0.0 => Ok(Some(*p)),
            _ => invalid(format!(
                "prices_per_million.{key} must be a non-negative number"
            )),
        }
    };
    let prices = PricesPerMillion {
        input: price("input", true)?.unwrap_or(0.0),
        output: price("output", true)?.unwrap_or(0.0),
        cache_read: price("cache_read", false)?,
        cache_write: price("cache_write", false)?,
    };
    let stated = |map: &JsonObject, key: &str| map.get(key).is_some_and(|v| !v.is_null());
    if stated(map, "cost_usd") || stated(map, "cost_nano_usd") {
        return Ok(());
    }
    let Some(input) = count(map.get("input_tokens")) else {
        return Ok(());
    };
    let cache_read = count(map.get("cache_read_input_tokens")).unwrap_or(0);
    let cache_write = count(map.get("cache_write_input_tokens")).unwrap_or(0);
    let input = if map.get("input_basis").and_then(Json::as_str) == Some("excludes_cache") {
        input.saturating_add(cache_read).saturating_add(cache_write)
    } else {
        input
    };
    let embeddings = map.get("operation_name").and_then(Json::as_str) == Some("embeddings");
    let output = match count(map.get("output_tokens")) {
        Some(output)
            if map.get("output_basis").and_then(Json::as_str) == Some("excludes_reasoning") =>
        {
            output.saturating_add(count(map.get("reasoning_output_tokens")).unwrap_or(0))
        }
        Some(output) => output,
        None if embeddings => 0,
        None => return Ok(()),
    };
    let tokens = BillableTokens {
        input,
        cache_read,
        cache_write,
        output,
    };
    if let Some(nanos) = estimated_cost_nano_usd(&prices, &tokens) {
        map.insert("cost_nano_usd".into(), Json::Number(nanos as f64));
        if !stated(map, "cost_source") {
            map.insert("cost_source".into(), Json::String("estimated".into()));
        }
    }
    Ok(())
}

/// `observation` with its [`OBSERVATION_EXTENSIONS`] expanded into the
/// members `profile` defines, in this order:
///
/// 1. `duration_ns`, `time_to_first_chunk_ns` and `output_chunk_intervals_ns` (whole nanoseconds,
///    which need no 17-digit doubles) set `duration_seconds`, `time_to_first_chunk_seconds` and
///    `output_chunk_intervals_seconds` when those are not stated.
/// 2. `zero_is_unreported` (`true` for [`ZERO_IS_UNREPORTED_DEFAULT`], or a list of counts) removes
///    the counts that are zero, as an SDK or proxy that fills unknown counts with zero means them.
/// 3. `failure` (`{"status", "no_response", "timed_out", "name"}`) sets `error_type`, as
///    [`error_type_for_failure`] derives it, when `error_type` is not stated.
/// 4. `prices_per_million` (`{"input", "output", "cache_read", "cache_write"}`), for a provider
///    attempt, sets `cost_nano_usd` and `cost_source: estimated` from the token counts, as
///    [`estimated_cost_nano_usd`] computes it, when no cost is stated. Input that excludes cache
///    has the cache added back; output that excludes reasoning has it added.
///
/// An observation with none of them is returned as it is, without a copy.
pub fn expand_observation(
    profile: Profile,
    observation: &Json,
) -> Result<Cow<'_, Json>, MetricError> {
    let Json::Object(members) = observation else {
        return Ok(Cow::Borrowed(observation));
    };
    if !OBSERVATION_EXTENSIONS
        .iter()
        .any(|key| members.contains_key(*key))
    {
        return Ok(Cow::Borrowed(observation));
    }
    let mut map = members.clone();
    seconds_from_ns(&mut map, "duration_ns", "duration_seconds")?;
    seconds_from_ns(
        &mut map,
        "time_to_first_chunk_ns",
        "time_to_first_chunk_seconds",
    )?;
    seconds_from_ns(
        &mut map,
        "output_chunk_intervals_ns",
        "output_chunk_intervals_seconds",
    )?;
    if let Some(setting) = map.remove("zero_is_unreported") {
        zeros_unreported(&mut map, setting)?;
    }
    if let Some(failure) = map.remove("failure") {
        failure_error_type(&mut map, failure)?;
    }
    if let Some(prices) = map.remove("prices_per_million")
        && profile == Profile::ProviderAttempt
    {
        priced_cost(&mut map, prices)?;
    }
    Ok(Cow::Owned(Json::Object(map)))
}

/// [`crate::project_result`] for an observation that may state
/// [`OBSERVATION_EXTENSIONS`], expanded first by [`expand_observation`].
pub fn project_extended(
    profile: Profile,
    observation: &Json,
) -> Result<crate::Projection, MetricError> {
    let expanded = expand_observation(profile, observation)?;
    crate::project_result(profile, expanded.as_ref())
}

/// A metric of an OpenTelemetry GenAI exporter as the profiles name it: the
/// profile metric, the profile that defines it, and its attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslatedMetric {
    pub name: &'static str,
    pub profile: Profile,
    pub attributes: crate::Attributes,
}

/// Prometheus label names of the attributes [`translate_otel_genai_metric`]
/// reads, with the OpenTelemetry names they stand for.
const PROMETHEUS_ATTRIBUTES: [(&str, &str); 8] = [
    ("error_type", "error.type"),
    ("gen_ai_operation_name", "gen_ai.operation.name"),
    ("gen_ai_provider_name", "gen_ai.provider.name"),
    ("gen_ai_request_model", "gen_ai.request.model"),
    ("gen_ai_response_model", "gen_ai.response.model"),
    ("gen_ai_system", "gen_ai.system"),
    ("gen_ai_token_type", "gen_ai.token.type"),
    ("server_address", "server.address"),
];

/// The profile metric an OpenTelemetry GenAI metric stands for, as a proxy
/// that relabels OpenTelemetry or Prometheus aggregates (Envoy AI Gateway)
/// reports them:
///
/// - the removed `gen_ai.client.token.usage` with `gen_ai.token.type` `input` or `output` is
///   `gen_ai.client.inference.usage.input_tokens` or `..output_tokens` (its sum is the Counter's
///   value; `PORTABLE-METRICS.md` replaced it with these Counters);
/// - a name a profile defines is kept;
/// - the Prometheus spelling (`gen_ai_client_token_usage_sum`, `gen_ai_token_type`, ...) is read as
///   the OpenTelemetry one, `_sum` and `_total` suffixes dropped; a `_count` or `_bucket` series
///   has no profile metric.
///
/// Attributes: `gen_ai.system` becomes `gen_ai.provider.name` unless that is
/// stated; the provider name is canonicalized by [`crate::gen_ai_provider_name`];
/// `gen_ai.token.type` is dropped; the rest are kept. `None` for any other
/// metric.
pub fn translate_otel_genai_metric(
    name: &str,
    attributes: &crate::Attributes,
) -> Option<TranslatedMetric> {
    let mut name = name.trim();
    let prometheus = !name.contains('.');
    if prometheus {
        if name.ends_with("_count") || name.ends_with("_bucket") {
            return None;
        }
        name = name
            .strip_suffix("_sum")
            .or_else(|| name.strip_suffix("_total"))
            .unwrap_or(name);
    }
    let mut attrs: crate::Attributes = attributes
        .iter()
        .map(|(key, value)| {
            let key = PROMETHEUS_ATTRIBUTES
                .iter()
                .find(|(label, _)| prometheus && label == key)
                .map_or(key.as_str(), |(_, dotted)| *dotted);
            (key.to_string(), value.clone())
        })
        .collect();
    let token_type = attrs.remove("gen_ai.token.type");
    let translated = match name {
        "gen_ai.client.token.usage" | "gen_ai_client_token_usage" => match token_type.as_deref() {
            Some("input") => "gen_ai.client.inference.usage.input_tokens",
            Some("output") => "gen_ai.client.inference.usage.output_tokens",
            _ => return None,
        },
        name => {
            let dotted = if prometheus {
                Profile::ALL
                    .iter()
                    .flat_map(|p| p.metric_names())
                    .find(|m| m.replace('.', "_") == name)?
            } else {
                Profile::ALL
                    .iter()
                    .flat_map(|p| p.metric_names())
                    .find(|m| **m == name)?
            };
            dotted
        }
    };
    let profile = *Profile::ALL
        .iter()
        .find(|p| p.metric_names().contains(&translated))?;
    if let Some(system) = attrs.remove("gen_ai.system") {
        attrs.entry("gen_ai.provider.name".into()).or_insert(system);
    }
    if let Some(provider) = attrs.get("gen_ai.provider.name") {
        match crate::gen_ai_provider_name(provider) {
            Some(canonical) => {
                attrs.insert("gen_ai.provider.name".into(), canonical);
            }
            None => {
                attrs.remove("gen_ai.provider.name");
            }
        }
    }
    Some(TranslatedMetric {
        name: translated,
        profile,
        attributes: attrs,
    })
}
