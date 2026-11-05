// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Reading what a model provider reported: the token usage of a response or
//! stream, the model and service tier it named, the account and project its
//! headers name, and its `gen_ai.provider.name`.
//!
//! Every gateway and integration needs these before it can describe a call
//! as an observation, and each provider reports them in its own shape. The
//! rules here are the ones the LiteLLM integration, the Kong plugin and
//! Datadog's AI gateways share, so that one response gives one observation
//! wherever it was read.
//!
//! A count the provider did not report is `None`, never zero. Nothing here
//! reads prompts or generated content into a value: only usage objects, model
//! names, service tiers and terminal events are kept.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::json::{Json, Prune};
use crate::number::MAX_SAFE_INTEGER;
use crate::observation::{
    ProviderAttemptObservation, TokenBreakdownObservation, is_context_band, is_identifier,
};
use crate::point::Attributes;
use crate::usage::ReportedUsage;

/// Provider names that differ from their OpenTelemetry well-known
/// `gen_ai.provider.name`, as gateways and SDKs spell them.
const PROVIDER_ALIASES: [(&str, &str); 21] = [
    ("anthropic_text", "anthropic"),
    ("aws-bedrock", "aws.bedrock"),
    ("aws_bedrock", "aws.bedrock"),
    ("azure", "azure.ai.openai"),
    ("azure_ai", "azure.ai.inference"),
    ("azure_openai", "azure.ai.openai"),
    ("azure_text", "azure.ai.openai"),
    ("bedrock", "aws.bedrock"),
    ("bedrock_converse", "aws.bedrock"),
    ("cohere_chat", "cohere"),
    // The Gemini API (Google AI Studio). Gemini served by Vertex AI is
    // `vertex_ai`.
    ("gemini", "gcp.gemini"),
    ("mistral", "mistral_ai"),
    ("moonshot", "moonshot_ai"),
    ("text-completion-openai", "openai"),
    ("vertex", "gcp.vertex_ai"),
    ("vertex_ai", "gcp.vertex_ai"),
    ("vertex_ai_beta", "gcp.vertex_ai"),
    ("watsonx", "ibm.watsonx.ai"),
    ("watsonx_text", "ibm.watsonx.ai"),
    ("x-ai", "x_ai"),
    ("xai", "x_ai"),
];

/// The `gen_ai.provider.name` of a provider as a gateway or SDK names it.
///
/// A name with an OpenTelemetry well-known value is mapped to it (`bedrock`
/// is `aws.bedrock`, `azure` is `azure.ai.openai`, `vertex_ai` is
/// `gcp.vertex_ai`, `gemini` is `gcp.gemini`, `xai` is `x_ai`, ...). Any other
/// name is kept as a low-cardinality identifier: trimmed, lower-cased, with
/// each character outside `[a-z0-9_.-]` replaced by `_`. `None` when that
/// does not start with a letter.
pub fn gen_ai_provider_name(name: &str) -> Option<String> {
    let lowered = name.trim().to_ascii_lowercase();
    if let Some((_, known)) = PROVIDER_ALIASES.iter().find(|(alias, _)| *alias == lowered) {
        return Some((*known).to_string());
    }
    let identifier: String = lowered
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    is_identifier(&identifier).then_some(identifier)
}

/// Whether `value` is a service tier an observation can carry: the
/// provider's own word, `^[A-Za-z0-9._-]{1,32}$`.
pub fn is_service_tier(value: &str) -> bool {
    is_context_band(value)
}

/// The shape a provider reported its usage in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageFormat {
    /// Told apart by the members of the usage object, as an SDK that
    /// normalizes several providers hands it over.
    Auto,
    /// OpenAI Chat Completions, Completions and Embeddings (`prompt_tokens`,
    /// `completion_tokens`), and the providers compatible with it.
    OpenAiChat,
    /// OpenAI Responses (`input_tokens`, `output_tokens`, both inclusive).
    OpenAiResponses,
    /// Anthropic Messages, also served by Bedrock InvokeModel and Vertex AI
    /// rawPredict: `input_tokens` leaves cache reads and writes out.
    Anthropic,
    /// Gemini `generateContent` (`usageMetadata`), from the Gemini API or
    /// Vertex AI.
    Gemini,
    /// Google's OpenAI-compatible Gemini endpoint, whose `completion_tokens`
    /// leaves thinking out while `total_tokens` counts it.
    GeminiOpenAiCompat,
}

impl UsageFormat {
    pub const ALL: [UsageFormat; 6] = [
        UsageFormat::Auto,
        UsageFormat::OpenAiChat,
        UsageFormat::OpenAiResponses,
        UsageFormat::Anthropic,
        UsageFormat::Gemini,
        UsageFormat::GeminiOpenAiCompat,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            UsageFormat::Auto => "auto",
            UsageFormat::OpenAiChat => "openai_chat",
            UsageFormat::OpenAiResponses => "openai_responses",
            UsageFormat::Anthropic => "anthropic",
            UsageFormat::Gemini => "gemini",
            UsageFormat::GeminiOpenAiCompat => "gemini_openai_compat",
        }
    }

    pub fn from_name(name: &str) -> Option<UsageFormat> {
        UsageFormat::ALL.into_iter().find(|f| f.as_str() == name)
    }
}

/// The token usage a provider reported for one operation, as the members of
/// an observation. Every member is optional: `None` was not reported.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProviderUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// `includes_cache` or `excludes_cache`, set with `input_tokens`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_basis: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// `includes_reasoning` or `excludes_reasoning`, set with `output_tokens`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_basis: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_5m_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_1h_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<u64>,
    /// The provider's own parts by modality (`image`, `audio`), zeros
    /// included. Text is never read: several SDKs fill it in themselves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens_by_modality: Option<BTreeMap<String, u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens_by_modality: Option<BTreeMap<String, u64>>,
}

const INCLUDES_CACHE: &str = "includes_cache";
const EXCLUDES_CACHE: &str = "excludes_cache";
const INCLUDES_REASONING: &str = "includes_reasoning";
const EXCLUDES_REASONING: &str = "excludes_reasoning";

/// A count no double holds exactly is above every bound the profiles state,
/// so it is never read as one.
fn as_f64(count: u64) -> f64 {
    // Counts are at most MAX_SAFE_INTEGER (2^53 - 1), which f64 holds exactly.
    count as f64
}

impl ProviderUsage {
    /// Whether every count is absent or zero: what an SDK reports for usage
    /// it does not know. Cache counts are part of it, because Anthropic's
    /// input count leaves them out and reads zero for input served wholly
    /// from cache.
    pub fn is_placeholder(&self) -> bool {
        [
            self.input_tokens,
            self.output_tokens,
            self.cache_read_input_tokens,
            self.cache_write_input_tokens,
        ]
        .iter()
        .all(|count| count.unwrap_or(0) == 0)
    }

    /// Merge the usage of a later event of the same stream. A count reported
    /// by both is the larger, as stream counts are cumulative and an early
    /// event can carry a placeholder.
    /// A basis goes with the count it describes: it is the later event's
    /// when that event's count is kept, so an early event of Google's
    /// OpenAI-compatible endpoint that had no thinking yet does not mark a
    /// later count that leaves thinking out as including it.
    pub fn merge(&mut self, later: &ProviderUsage) {
        fn basis(
            basis: &mut Option<&'static str>,
            later_basis: Option<&'static str>,
            count: Option<u64>,
            later_count: Option<u64>,
        ) {
            let later_kept = match (count, later_count) {
                (Some(a), Some(b)) => b >= a,
                (None, Some(_)) => true,
                (_, None) => false,
            };
            *basis = if later_kept {
                later_basis.or(*basis)
            } else {
                basis.or(later_basis)
            };
        }
        basis(
            &mut self.input_basis,
            later.input_basis,
            self.input_tokens,
            later.input_tokens,
        );
        basis(
            &mut self.output_basis,
            later.output_basis,
            self.output_tokens,
            later.output_tokens,
        );
        fn larger(earlier: &mut Option<u64>, later: Option<u64>) {
            *earlier = match (*earlier, later) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
        }
        larger(&mut self.input_tokens, later.input_tokens);
        larger(&mut self.output_tokens, later.output_tokens);
        larger(
            &mut self.cache_read_input_tokens,
            later.cache_read_input_tokens,
        );
        larger(
            &mut self.cache_write_input_tokens,
            later.cache_write_input_tokens,
        );
        larger(
            &mut self.cache_write_5m_input_tokens,
            later.cache_write_5m_input_tokens,
        );
        larger(
            &mut self.cache_write_1h_input_tokens,
            later.cache_write_1h_input_tokens,
        );
        larger(
            &mut self.reasoning_output_tokens,
            later.reasoning_output_tokens,
        );
        if later.input_tokens_by_modality.is_some() {
            self.input_tokens_by_modality = later.input_tokens_by_modality.clone();
        }
        if later.output_tokens_by_modality.is_some() {
            self.output_tokens_by_modality = later.output_tokens_by_modality.clone();
        }
    }

    fn reported(&self) -> ReportedUsage {
        let parts = |parts: &Option<BTreeMap<String, u64>>| {
            parts.as_ref().map(|parts| {
                parts
                    .iter()
                    .map(|(name, count)| (name.clone(), as_f64(*count)))
                    .collect()
            })
        };
        ReportedUsage {
            input_tokens: self.input_tokens.map(as_f64),
            output_tokens: self.output_tokens.map(as_f64),
            cache_read_input_tokens: self.cache_read_input_tokens.map(as_f64),
            cache_write_input_tokens: self.cache_write_input_tokens.map(as_f64),
            cache_write_5m_input_tokens: self.cache_write_5m_input_tokens.map(as_f64),
            cache_write_1h_input_tokens: self.cache_write_1h_input_tokens.map(as_f64),
            reasoning_output_tokens: self.reasoning_output_tokens.map(as_f64),
            input_basis: self.input_basis.map(str::to_string),
            output_basis: self.output_basis.map(str::to_string),
            input_tokens_by_modality: parts(&self.input_tokens_by_modality),
            output_tokens_by_modality: parts(&self.output_tokens_by_modality),
            ..ReportedUsage::default()
        }
    }

    /// Set the usage members of a provider attempt observation, keeping its
    /// server tool counts.
    pub fn apply_to_attempt(&self, observation: &mut ProviderAttemptObservation) {
        let reported = self.reported();
        observation.usage = ReportedUsage {
            web_search_requests: observation.usage.web_search_requests,
            web_search_requests_secondary: observation.usage.web_search_requests_secondary,
            ..reported
        };
    }

    /// Set the usage members of a token breakdown observation, which has no
    /// modality parts.
    pub fn apply_to_breakdown(&self, observation: &mut TokenBreakdownObservation) {
        observation.usage = ReportedUsage {
            input_tokens_by_modality: None,
            output_tokens_by_modality: None,
            ..self.reported()
        };
    }
}

/// What one provider response, or one stream event, reported.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProviderResponse {
    /// `None` when the payload had no usage, or only a placeholder of zeros.
    pub usage: Option<ProviderUsage>,
    /// The model the provider named (`gen_ai.response.model`).
    pub response_model: Option<String>,
    /// The tier the provider served the operation at, in its own words. The
    /// tier a request asked for is never read.
    pub service_tier: Option<String>,
    /// The error the provider reported in the body or in the stream, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ProviderError>,
    /// Why something the provider reported was not kept, such as
    /// [`SERVICE_TIER_DROPPED`]. Empty when everything was kept.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<&'static str>,
}

/// The issue of a response whose service tier is outside
/// `^[A-Za-z0-9._-]{1,32}$`: the tier is not kept, as the profiles require,
/// and this says why it is missing.
pub const SERVICE_TIER_DROPPED: &str = "service_tier_dropped";

/// The longest provider error message kept, in bytes.
pub const MAX_ERROR_MESSAGE_BYTES: usize = 256;

/// The longest provider error code kept, in bytes.
const MAX_ERROR_CODE_BYTES: usize = 64;

/// An error a provider reported, as a gateway logs or classifies it.
/// Every member is cut short and holds no control characters, so it can be
/// logged as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProviderError {
    /// The provider's name for the failure (its `type`, else `code`, else
    /// `status`), to classify with [`crate::classify_error`].
    pub name: Option<String>,
    /// The provider's `code`, a string or the digits of a number.
    pub code: Option<String>,
    /// The provider's message, cut to [`MAX_ERROR_MESSAGE_BYTES`] at a
    /// character boundary.
    pub message: Option<String>,
}

impl ProviderResponse {
    /// Merge a later event of the same stream: usage as
    /// [`ProviderUsage::merge`], the first model, tier and error named.
    pub fn merge(&mut self, later: ProviderResponse) {
        match (&mut self.usage, later.usage) {
            (Some(usage), Some(next)) => usage.merge(&next),
            (usage @ None, next) => *usage = next,
            (Some(_), None) => {}
        }
        if self.response_model.is_none() {
            self.response_model = later.response_model;
        }
        if self.service_tier.is_none() {
            self.service_tier = later.service_tier;
        }
        if self.error.is_none() {
            self.error = later.error;
        }
        for issue in later.issues {
            if !self.issues.contains(&issue) {
                self.issues.push(issue);
            }
        }
        if self.service_tier.is_some() {
            // A tier was kept after all, from another event.
            self.issues.retain(|issue| *issue != SERVICE_TIER_DROPPED);
        }
    }
}

/// `text` without control characters, trimmed, and cut to `max` bytes at a
/// character boundary; `None` when nothing is left.
fn safe_text(text: &str, max: usize) -> Option<String> {
    let mut out = String::with_capacity(text.len().min(max));
    for c in text.trim().chars() {
        let c = if c.is_control() { ' ' } else { c };
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    let out = out.trim_end();
    (!out.is_empty()).then(|| out.to_string())
}

/// A code as text: a string, or the digits of a whole number.
fn code_text(value: Option<&Json>) -> Option<String> {
    match value? {
        Json::String(code) => safe_text(code, MAX_ERROR_CODE_BYTES),
        number @ Json::Number(_) => count(Some(number)).map(|code| code.to_string()),
        _ => None,
    }
}

/// The error a payload reports: Responses `response.failed` (under
/// `response.error`), an `error` event (Anthropic nests it under `error`;
/// the Responses one holds `code` and `message` beside its type), or an
/// `{"error": ...}` payload that carries no content. `None` otherwise.
fn provider_error(payload: &Json) -> Option<ProviderError> {
    let kind = member(Some(payload), "type").and_then(Json::as_str);
    let content = ["choices", "candidates"]
        .iter()
        .any(|key| member(Some(payload), key).is_some());
    let object = match kind {
        Some("response.failed") => member(member(Some(payload), "response"), "error"),
        Some("error") => member(Some(payload), "error")
            .filter(|e| e.as_object().is_some())
            .or(Some(payload)),
        _ if content => None,
        _ => member(Some(payload), "error"),
    };
    let object = object?;
    if let Some(message) = object.as_str() {
        // `{"error": "message"}`, as some compatible servers send it.
        return Some(ProviderError {
            message: safe_text(message, MAX_ERROR_MESSAGE_BYTES),
            ..ProviderError::default()
        });
    }
    object.as_object()?;
    let mut name = error_name(Some(object));
    if kind == Some("error") && std::ptr::eq(object, payload) {
        // The Responses error event: its own `type` is `error`.
        name = text(member(Some(object), "code"));
    }
    let error = ProviderError {
        name: name.and_then(|name| safe_text(&name, MAX_ERROR_CODE_BYTES)),
        code: code_text(member(Some(object), "code")),
        message: member(Some(object), "message")
            .and_then(Json::as_str)
            .and_then(|message| safe_text(message, MAX_ERROR_MESSAGE_BYTES)),
    };
    (error != ProviderError::default()).then_some(error)
}

/// A token count: a whole, non-negative JSON number no larger than
/// `MAX_SAFE_INTEGER`. Anything else is not a count.
fn count(value: Option<&Json>) -> Option<u64> {
    let number = value?.as_f64()?;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 {
        return None;
    }
    // Whole and within [0, 2^53), so the conversion is exact.
    let whole = number as u64;
    (whole <= MAX_SAFE_INTEGER).then_some(whole)
}

fn member<'a>(object: Option<&'a Json>, key: &str) -> Option<&'a Json> {
    object?.get(key).filter(|value| !value.is_null())
}

fn text(value: Option<&Json>) -> Option<String> {
    value?
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn sum(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (a, b) => a.unwrap_or(0).checked_add(b.unwrap_or(0)),
    }
}

/// The usage object of a payload: a response body, or a stream event that
/// nests it under `response` (Responses) or `message` (Anthropic
/// `message_start`). A payload that is itself a usage object is read as one.
fn usage_object(payload: &Json) -> Option<&Json> {
    if let Some(usage) = member(Some(payload), "usage") {
        return usage.as_object().map(|_| usage);
    }
    for parent in ["response", "message"] {
        if let Some(usage) = member(member(Some(payload), parent), "usage") {
            return usage.as_object().map(|_| usage);
        }
    }
    let object = payload.as_object()?;
    [
        "prompt_tokens",
        "completion_tokens",
        "input_tokens",
        "output_tokens",
    ]
    .iter()
    .any(|key| object.contains_key(*key))
    .then_some(payload)
}

fn gemini_usage_object(payload: &Json) -> Option<&Json> {
    if let Some(usage) = member(Some(payload), "usageMetadata") {
        return usage.as_object().map(|_| usage);
    }
    let object = payload.as_object()?;
    [
        "promptTokenCount",
        "candidatesTokenCount",
        "totalTokenCount",
    ]
    .iter()
    .any(|key| object.contains_key(*key))
    .then_some(payload)
}

/// The format of an `Auto` payload, from the members of its usage object.
fn detect(payload: &Json, usage: Option<&Json>) -> UsageFormat {
    if gemini_usage_object(payload).is_some() {
        return UsageFormat::Gemini;
    }
    let has = |key: &str| member(usage, key).is_some();
    if has("prompt_tokens") || has("completion_tokens") {
        return UsageFormat::OpenAiChat;
    }
    let event = member(Some(payload), "type").and_then(Json::as_str);
    if has("cache_read_input_tokens")
        || has("cache_creation_input_tokens")
        || matches!(event, Some("message" | "message_start" | "message_delta"))
    {
        return UsageFormat::Anthropic;
    }
    UsageFormat::OpenAiResponses
}

/// The `image` and `audio` parts of an OpenAI usage details object.
fn openai_modalities(details: Option<&Json>) -> Option<BTreeMap<String, u64>> {
    let mut parts = BTreeMap::new();
    for modality in ["image", "audio"] {
        if let Some(value) = count(member(details, &format!("{modality}_tokens"))) {
            parts.insert(modality.to_string(), value);
        }
    }
    (!parts.is_empty()).then_some(parts)
}

/// The `IMAGE` and `AUDIO` parts of a Gemini `*TokensDetails` list.
fn gemini_modalities(details: Option<&Json>) -> Option<BTreeMap<String, u64>> {
    let mut parts: BTreeMap<String, u64> = BTreeMap::new();
    for entry in details?.as_array()? {
        let modality = match member(Some(entry), "modality").and_then(Json::as_str) {
            Some("IMAGE") => "image",
            Some("AUDIO") => "audio",
            _ => continue,
        };
        let Some(value) = count(member(Some(entry), "tokenCount")) else {
            continue;
        };
        let part = parts.entry(modality.to_string()).or_insert(0);
        *part = part.saturating_add(value);
    }
    (!parts.is_empty()).then_some(parts)
}

/// Chat Completions and Responses usage: both totals are inclusive.
fn openai_usage(usage: Option<&Json>, responses: bool, embeddings: bool) -> ProviderUsage {
    let (input_key, output_key, input_details_key, output_details_key) = if responses {
        (
            "input_tokens",
            "output_tokens",
            "input_tokens_details",
            "output_tokens_details",
        )
    } else {
        (
            "prompt_tokens",
            "completion_tokens",
            "prompt_tokens_details",
            "completion_tokens_details",
        )
    };
    let input_details = member(usage, input_details_key);
    let output_details = member(usage, output_details_key);
    let input_tokens = count(member(usage, input_key));
    let output_tokens = if embeddings {
        None
    } else {
        count(member(usage, output_key))
    };
    // An SDK that normalizes Anthropic usage into this shape keeps its cache
    // counts at the top level.
    let cache_read = count(member(usage, "cache_read_input_tokens"))
        .or_else(|| count(member(input_details, "cached_tokens")));
    let cache_write = count(member(usage, "cache_creation_input_tokens"))
        .or_else(|| count(member(input_details, "cache_creation_tokens")))
        .or_else(|| count(member(input_details, "cache_write_tokens")));
    let lifetimes = member(input_details, "cache_creation_token_details")
        .or_else(|| member(usage, "cache_creation"));
    let reasoning = if embeddings {
        None
    } else {
        count(member(output_details, "reasoning_tokens"))
    };
    // OpenAI's completion count includes reasoning, so the prompt and the
    // completion add up to the total. Some compatible endpoints (Vertex AI's
    // OpenAI-compatible API) leave reasoning out of the completion and count
    // it in the total: then the prompt, completion and reasoning add up to
    // it, and the completion excludes reasoning.
    let total = count(member(usage, "total_tokens"));
    let reasoning_separate = !responses
        && matches!(
            (input_tokens, output_tokens, reasoning, total),
            (Some(prompt), Some(completion), Some(thinking), Some(total))
                if thinking > 0
                    && prompt.checked_add(completion).and_then(|n| n.checked_add(thinking))
                        == Some(total)
        );
    let output_basis = if reasoning_separate {
        EXCLUDES_REASONING
    } else {
        INCLUDES_REASONING
    };
    ProviderUsage {
        input_basis: input_tokens.map(|_| INCLUDES_CACHE),
        input_tokens,
        output_basis: output_tokens.map(|_| output_basis),
        output_tokens,
        cache_read_input_tokens: cache_read,
        cache_write_input_tokens: cache_write,
        cache_write_5m_input_tokens: count(member(lifetimes, "ephemeral_5m_input_tokens")),
        cache_write_1h_input_tokens: count(member(lifetimes, "ephemeral_1h_input_tokens")),
        reasoning_output_tokens: reasoning,
        input_tokens_by_modality: openai_modalities(input_details),
        output_tokens_by_modality: if embeddings {
            None
        } else {
            openai_modalities(output_details)
        },
    }
}

/// Google's OpenAI-compatible Gemini endpoint: what `total_tokens` counts
/// beyond the prompt and completion is the thinking the completion left out.
fn gemini_compat_usage(usage: Option<&Json>, embeddings: bool) -> ProviderUsage {
    let mut parsed = openai_usage(usage, false, embeddings);
    if embeddings || parsed.reasoning_output_tokens.is_some() {
        return parsed;
    }
    let (Some(prompt), Some(completion), Some(total)) = (
        parsed.input_tokens,
        parsed.output_tokens,
        count(member(usage, "total_tokens")),
    ) else {
        return parsed;
    };
    if let Some(thinking) = total
        .checked_sub(prompt)
        .and_then(|rest| rest.checked_sub(completion))
        .filter(|thinking| *thinking > 0)
    {
        parsed.reasoning_output_tokens = Some(thinking);
        parsed.output_basis = Some(EXCLUDES_REASONING);
    }
    parsed
}

/// Anthropic Messages usage: `input_tokens` leaves cache reads and writes
/// out, and `output_tokens` counts thinking.
fn anthropic_usage(usage: Option<&Json>, embeddings: bool) -> ProviderUsage {
    let input_tokens = count(member(usage, "input_tokens"));
    let output_tokens = if embeddings {
        None
    } else {
        count(member(usage, "output_tokens"))
    };
    let lifetimes = member(usage, "cache_creation");
    ProviderUsage {
        input_basis: input_tokens.map(|_| EXCLUDES_CACHE),
        input_tokens,
        output_basis: output_tokens.map(|_| INCLUDES_REASONING),
        output_tokens,
        cache_read_input_tokens: count(member(usage, "cache_read_input_tokens")),
        cache_write_input_tokens: count(member(usage, "cache_creation_input_tokens")),
        cache_write_5m_input_tokens: count(member(lifetimes, "ephemeral_5m_input_tokens")),
        cache_write_1h_input_tokens: count(member(lifetimes, "ephemeral_1h_input_tokens")),
        ..ProviderUsage::default()
    }
}

/// Gemini `usageMetadata`. Billed input is the prompt and the tool-use
/// prompt, which include the cached content. Billed output is the candidates
/// and the thoughts, except when the candidates already count the thoughts:
/// then the prompt, candidates and tool-use prompt add up to the total.
fn gemini_usage(usage: Option<&Json>, embeddings: bool) -> ProviderUsage {
    let prompt = count(member(usage, "promptTokenCount"));
    let tool_prompt = count(member(usage, "toolUsePromptTokenCount"));
    let candidates = count(member(usage, "candidatesTokenCount"));
    let thoughts = count(member(usage, "thoughtsTokenCount"));
    let total = count(member(usage, "totalTokenCount"));
    let thoughts_in_candidates = thoughts.is_some_and(|t| t > 0)
        && total.is_some()
        && sum(sum(prompt, candidates), tool_prompt) == total;
    let output_tokens = if embeddings {
        None
    } else if thoughts_in_candidates {
        candidates
    } else {
        sum(candidates, thoughts)
    };
    let input_tokens = if prompt.is_some() {
        sum(prompt, tool_prompt)
    } else {
        // Vertex AI leaves the prompt count out when the whole prompt was
        // served from cache; the total still counts it.
        total.and_then(|total| total.checked_sub(output_tokens.unwrap_or(0)))
    };
    ProviderUsage {
        input_basis: input_tokens.map(|_| INCLUDES_CACHE),
        input_tokens,
        output_basis: output_tokens.map(|_| INCLUDES_REASONING),
        output_tokens,
        cache_read_input_tokens: count(member(usage, "cachedContentTokenCount")),
        reasoning_output_tokens: if embeddings { None } else { thoughts },
        input_tokens_by_modality: gemini_modalities(member(usage, "promptTokensDetails")),
        output_tokens_by_modality: if embeddings {
            None
        } else {
            gemini_modalities(member(usage, "candidatesTokensDetails"))
        },
        ..ProviderUsage::default()
    }
}

fn response_model(payload: &Json) -> Option<String> {
    text(member(Some(payload), "model"))
        .or_else(|| text(member(member(Some(payload), "response"), "model")))
        .or_else(|| text(member(member(Some(payload), "message"), "model")))
        .or_else(|| text(member(Some(payload), "modelVersion")))
}

/// The served tier the payload names, and whether it named one outside the
/// grammar, which is not kept.
fn service_tier(payload: &Json, usage: Option<&Json>) -> (Option<String>, bool) {
    let named = [
        member(Some(payload), "service_tier"),
        member(member(Some(payload), "response"), "service_tier"),
        member(usage, "service_tier"),
    ]
    .into_iter()
    .find_map(text);
    match named {
        Some(tier) if is_service_tier(&tier) => (Some(tier), false),
        Some(_) => (None, true),
        None => (None, false),
    }
}

/// Read a provider response body, or one stream event, in `format`.
/// `embeddings` is `true` for an embeddings operation, which has no output
/// side. A Gemini stream returned as one JSON array is read as its events.
pub fn parse_provider_response(
    format: UsageFormat,
    payload: &Json,
    embeddings: bool,
) -> ProviderResponse {
    if let Some(events) = payload.as_array() {
        let mut merged = ProviderResponse::default();
        for event in events {
            merged.merge(parse_provider_response(format, event, embeddings));
        }
        return merged;
    }
    let usage = usage_object(payload);
    let format = match format {
        UsageFormat::Auto => detect(payload, usage),
        format => format,
    };
    let parsed = match format {
        UsageFormat::Gemini => {
            gemini_usage_object(payload).map(|usage| gemini_usage(Some(usage), embeddings))
        }
        UsageFormat::Anthropic => usage.map(|usage| {
            let mut parsed = anthropic_usage(Some(usage), embeddings);
            // `message_start` states an output count before any output: a
            // placeholder, not a count the provider reported. Only
            // `message_delta` reports output, so a stream cut before it has
            // no output count and the caller estimates one.
            if member(Some(payload), "type").and_then(Json::as_str) == Some("message_start") {
                parsed.output_tokens = None;
                parsed.output_basis = None;
            }
            parsed
        }),
        UsageFormat::GeminiOpenAiCompat => {
            usage.map(|usage| gemini_compat_usage(Some(usage), embeddings))
        }
        UsageFormat::OpenAiResponses => {
            usage.map(|usage| openai_usage(Some(usage), true, embeddings))
        }
        UsageFormat::OpenAiChat | UsageFormat::Auto => {
            usage.map(|usage| openai_usage(Some(usage), false, embeddings))
        }
    };
    let (service_tier, tier_dropped) = service_tier(payload, usage);
    ProviderResponse {
        usage: parsed.filter(|usage| !usage.is_placeholder()),
        response_model: response_model(payload),
        service_tier,
        error: provider_error(payload),
        issues: if tier_dropped {
            vec![SERVICE_TIER_DROPPED]
        } else {
            Vec::new()
        },
    }
}

/// [`parse_provider_response`] for the usage alone.
pub fn parse_provider_usage(
    format: UsageFormat,
    payload: &Json,
    embeddings: bool,
) -> Option<ProviderUsage> {
    parse_provider_response(format, payload, embeddings).usage
}

/// How a provider stream ended, as far as its events say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamOutcome {
    /// No terminal event yet: a stream that ends here was cut short.
    Open,
    /// The provider's terminal event arrived: Anthropic `message_stop`,
    /// Responses `response.completed` or `response.incomplete`, Chat
    /// Completions `[DONE]`, or a Gemini candidate's `finishReason`.
    Completed,
    /// The provider reported a failure in the stream: Responses
    /// `response.failed`, an `error` event, or an `{"error": ...}` payload.
    Failed,
}

impl StreamOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            StreamOutcome::Open => "open",
            StreamOutcome::Completed => "completed",
            StreamOutcome::Failed => "failed",
        }
    }
}

/// The longest stream line kept, once the long strings it holds are cut to
/// [`MAX_KEPT_STRING_BYTES`]: a line whose structure alone is longer is
/// skipped. A line that carries a whole response, such as the Responses
/// `response.completed` event, is mostly generated text, so it is read
/// however long its text is.
pub const MAX_STREAM_LINE_BYTES: usize = 1024 * 1024;

/// The bytes of a JSON string kept as a line or body is read. A longer
/// string is generated content (a message, a tool argument, an image): the
/// rest of it is dropped as it passes, so it is neither held nor decoded.
/// Every value this module reads (a model, a tier, an error type or code)
/// is far shorter.
pub const MAX_KEPT_STRING_BYTES: usize = 1024;

/// A line or body shorter than this is decoded as it is, without cutting
/// its strings first: cutting would not save enough to pay for itself.
const CUT_ABOVE_BYTES: usize = 8 * 1024;

/// JSON text with every string cut to its first [`MAX_KEPT_STRING_BYTES`]
/// bytes, fed in pieces that can end anywhere. A string is cut only at a
/// character boundary outside an escape (never between the two halves of a
/// surrogate pair), so the text stays valid JSON, and valid UTF-8 when the
/// input was. Bytes outside strings are kept as they are.
#[derive(Debug, Clone, Default)]
struct CutStrings {
    out: Vec<u8>,
    in_string: bool,
    /// The previous byte was the backslash of an escape.
    escaped: bool,
    /// Hex digits of a `\u` escape still to come, and its value so far.
    hex_left: u8,
    hex: u32,
    /// The last escape was a high surrogate, whose low half must be kept.
    high_surrogate: bool,
    /// The bytes of the current string kept so far.
    kept: usize,
    /// The current string is being dropped until its closing quote.
    cutting: bool,
}

impl CutStrings {
    fn clear(&mut self) {
        self.out.clear();
        self.in_string = false;
        self.escaped = false;
        self.hex_left = 0;
        self.hex = 0;
        self.high_surrogate = false;
        self.kept = 0;
        self.cutting = false;
    }

    fn is_empty(&self) -> bool {
        self.out.is_empty()
    }

    fn push(&mut self, bytes: &[u8]) {
        let mut at = 0;
        while at < bytes.len() {
            if !self.in_string {
                // Copy up to the next quote in one go.
                let next = memchr::memchr(b'"', &bytes[at..]).map_or(bytes.len(), |n| at + n);
                self.out.extend_from_slice(&bytes[at..next]);
                if next < bytes.len() {
                    self.out.push(b'"');
                    self.in_string = true;
                    self.kept = 0;
                    self.cutting = false;
                    self.high_surrogate = false;
                }
                at = next + 1;
                continue;
            }
            if self.cutting {
                if self.escaped {
                    self.escaped = false;
                    at += 1;
                    continue;
                }
                // Skip to the closing quote, 64 bytes at a time, so text
                // dense with escapes (tool arguments, quoted prose) costs no
                // more than plain text.
                match closing_quote(&bytes[at..]) {
                    Ok(n) => {
                        self.out.push(b'"');
                        self.in_string = false;
                        at += n + 1;
                    }
                    Err(escaped) => {
                        // The escaped byte of a final backslash is in the
                        // next piece.
                        self.escaped = escaped;
                        at = bytes.len();
                    }
                }
                continue;
            }
            if !self.escaped && self.hex_left == 0 && self.kept < MAX_KEPT_STRING_BYTES {
                // Copy the plain run of a kept string in one go, up to the
                // next quote or backslash or the end of what is kept. A
                // plain character after an escape ends any surrogate pair.
                let end = bytes.len().min(at + (MAX_KEPT_STRING_BYTES - self.kept));
                let run = memchr::memchr2(b'"', b'\\', &bytes[at..end]).unwrap_or(end - at);
                if run > 0 {
                    self.out.extend_from_slice(&bytes[at..at + run]);
                    self.kept += run;
                    self.high_surrogate = false;
                    at += run;
                    continue;
                }
            }
            let b = bytes[at];
            at += 1;
            if self.escaped {
                self.escaped = false;
                if b == b'u' {
                    self.hex_left = 4;
                    self.hex = 0;
                } else {
                    self.high_surrogate = false;
                }
                self.keep(b);
                continue;
            }
            if self.hex_left > 0 {
                self.hex_left -= 1;
                self.hex = self.hex * 16 + char::from(b).to_digit(16).unwrap_or(0);
                if self.hex_left == 0 {
                    self.high_surrogate = (0xD800..0xDC00).contains(&self.hex);
                }
                self.keep(b);
                continue;
            }
            if b == b'"' {
                self.out.push(b);
                self.in_string = false;
                continue;
            }
            let boundary = b & 0xC0 != 0x80 && !(self.high_surrogate && b == b'\\');
            if self.kept >= MAX_KEPT_STRING_BYTES && boundary {
                self.cutting = true;
                self.escaped = b == b'\\';
                continue;
            }
            if b == b'\\' {
                self.escaped = true;
            } else if b & 0xC0 != 0x80 {
                self.high_surrogate = false;
            }
            self.keep(b);
        }
    }

    fn keep(&mut self, b: u8) {
        self.out.push(b);
        self.kept += 1;
    }
}

/// The bits of the quotes and of the backslashes of a 64-byte block, bit `i`
/// for byte `i`, eight bytes at a time.
fn quote_masks(block: &[u8]) -> (u64, u64) {
    const LOW: u64 = 0x7F7F_7F7F_7F7F_7F7F;
    const ONES: u64 = 0x0101_0101_0101_0101;
    /// Bit `k` set when byte `k` of `word` is zero: its high bit, found
    /// without carries between bytes, gathered by one multiplication.
    fn zero_bytes(word: u64) -> u64 {
        let high = !(((word & LOW).wrapping_add(LOW)) | word | LOW);
        (high >> 7).wrapping_mul(0x0102_0408_1020_4080) >> 56
    }
    let mut quotes = 0u64;
    let mut backslashes = 0u64;
    for (k, chunk) in block.as_chunks::<8>().0.iter().enumerate().take(8) {
        let word = u64::from_le_bytes(*chunk);
        quotes |= zero_bytes(word ^ (ONES * u64::from(b'"'))) << (8 * k);
        backslashes |= zero_bytes(word ^ (ONES * u64::from(b'\\'))) << (8 * k);
    }
    (quotes, backslashes)
}

/// The bits of the bytes a backslash escapes in a 64-byte block whose
/// backslashes are `backslashes`. `carry` is 1 when the first byte is
/// escaped by the block before, and is set for the block after: an odd run
/// of backslashes escapes the byte after it, an even run does not. This is
/// simdjson's escape scanner.
fn escaped_bits(backslashes: u64, carry: &mut u64) -> u64 {
    const EVEN: u64 = 0x5555_5555_5555_5555;
    let backslashes = backslashes & !*carry;
    let follows_escape = (backslashes << 1) | *carry;
    let odd_starts = backslashes & !EVEN & !follows_escape;
    let (even_starts, overflow) = odd_starts.overflowing_add(backslashes);
    *carry = u64::from(overflow);
    (EVEN ^ (even_starts << 1)) & follows_escape
}

/// The offset of the first quote no backslash escapes in `bytes`, the rest
/// of a JSON string with no escape pending at its start: the quote that
/// closes it. Without one, `Err` says whether `bytes` ends in an escape
/// whose escaped byte is still to come.
fn closing_quote(bytes: &[u8]) -> Result<usize, bool> {
    // Text without escapes is skipped at memchr's speed; from the first
    // backslash on, a block of 64 bytes costs the same however many escapes
    // it holds.
    let mut base = match memchr::memchr2(b'"', b'\\', bytes) {
        None => return Err(false),
        Some(n) if bytes[n] == b'"' => return Ok(n),
        Some(n) => n,
    };
    let mut carry = 0u64;
    let (blocks, remainder) = bytes[base..].as_chunks::<64>();
    for block in blocks {
        let (quotes, backslashes) = quote_masks(block);
        let open = quotes & !escaped_bits(backslashes, &mut carry);
        if open != 0 {
            return Ok(base + open.trailing_zeros() as usize);
        }
        base += 64;
    }
    let mut escaped = carry != 0;
    for (i, &b) in remainder.iter().enumerate() {
        if escaped {
            escaped = false;
        } else if b == b'"' {
            return Ok(base + i);
        } else if b == b'\\' {
            escaped = true;
        }
    }
    Err(escaped)
}

/// `text` with its long strings cut, as [`CutStrings`] does.
fn cut_strings(text: &[u8]) -> Vec<u8> {
    let mut cut = CutStrings::default();
    cut.push(text);
    cut.out
}

/// What a quoted name in a line says about whether to decode it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Candidate {
    /// An event type or terminal marker: always decode.
    Marker,
    /// A member worth decoding when its value is not `null` (or when the
    /// name is a value, as in `"type": "error"`).
    Member,
    /// The model, decoded until one is known.
    Model,
    /// The service tier, decoded until one is known.
    Tier,
}

/// The quoted names that make a line worth decoding. A quote inside a JSON
/// string is escaped, so generated text cannot contain them. OpenAI's
/// chunks name the model and tier and carry `"usage": null` in every chunk,
/// so once both are known a content chunk matches none of them.
const CANDIDATES: [(&str, Candidate); 12] = [
    ("\"usage\"", Candidate::Member),
    ("\"usageMetadata\"", Candidate::Member),
    ("\"error\"", Candidate::Member),
    ("\"model\"", Candidate::Model),
    ("\"service_tier\"", Candidate::Tier),
    ("\"finishReason\"", Candidate::Marker),
    ("\"message_start\"", Candidate::Marker),
    ("\"message_delta\"", Candidate::Marker),
    ("\"message_stop\"", Candidate::Marker),
    ("\"response.completed\"", Candidate::Marker),
    ("\"response.incomplete\"", Candidate::Marker),
    ("\"response.failed\"", Candidate::Marker),
];

/// What a stream event or response body is decoded for: the members this
/// module reads, and nothing of the generated content around them. Every
/// other member is checked as JSON but not built.
const EVENT: Prune = Prune::Members(&[
    ("type", Prune::All),
    ("code", Prune::All),
    ("model", Prune::All),
    ("modelVersion", Prune::All),
    ("service_tier", Prune::All),
    ("usage", Prune::All),
    ("usageMetadata", Prune::All),
    ("error", Prune::All),
    (
        "response",
        Prune::Members(&[
            ("usage", Prune::All),
            ("model", Prune::All),
            ("service_tier", Prune::All),
            ("error", Prune::All),
        ]),
    ),
    (
        "message",
        Prune::Members(&[("usage", Prune::All), ("model", Prune::All)]),
    ),
    // Each candidate's finish reason; each choice is only seen to be there.
    (
        "candidates",
        Prune::Members(&[("finishReason", Prune::All)]),
    ),
    ("choices", Prune::Members(&[])),
    // A payload that is itself a usage object.
    ("prompt_tokens", Prune::All),
    ("completion_tokens", Prune::All),
    ("total_tokens", Prune::All),
    ("input_tokens", Prune::All),
    ("output_tokens", Prune::All),
    ("prompt_tokens_details", Prune::All),
    ("completion_tokens_details", Prune::All),
    ("input_tokens_details", Prune::All),
    ("output_tokens_details", Prune::All),
    ("cache_read_input_tokens", Prune::All),
    ("cache_creation_input_tokens", Prune::All),
    ("cache_creation", Prune::All),
    ("promptTokenCount", Prune::All),
    ("candidatesTokenCount", Prune::All),
    ("totalTokenCount", Prune::All),
    ("thoughtsTokenCount", Prune::All),
    ("toolUsePromptTokenCount", Prune::All),
    ("cachedContentTokenCount", Prune::All),
    ("promptTokensDetails", Prune::All),
    ("candidatesTokensDetails", Prune::All),
]);

/// Whether the quoted name ending at `rest` is other than a member whose
/// value is `null`.
fn not_null_member(rest: &str) -> bool {
    match rest.trim_start().strip_prefix(':') {
        Some(value) => !value.trim_start().starts_with("null"),
        None => true,
    }
}

/// The server-sent events of a provider stream, read as they pass through:
/// usage, model and service tier merged across events, the terminal event,
/// and the name of an in-band error.
///
/// Lines end only at CR, LF or CRLF, as server-sent events define them, so a
/// JSON string holding U+2028 does not split an event. A chunk can end
/// anywhere: an unfinished line is held until the next chunk, its strings
/// cut to [`MAX_KEPT_STRING_BYTES`] as they arrive, so a line holding a whole
/// response is read without holding its text. Only lines that can carry
/// usage, the model, the tier or a terminal event are decoded.
#[derive(Debug, Clone)]
pub struct UsageStream {
    format: UsageFormat,
    embeddings: bool,
    response: ProviderResponse,
    outcome: StreamOutcome,
    error_name: Option<String>,
    pending: CutStrings,
    skipping: bool,
    /// The previous chunk ended in CR: a LF that starts the next one ends no
    /// line of its own.
    after_cr: bool,
}

impl UsageStream {
    pub fn new(format: UsageFormat, embeddings: bool) -> Self {
        Self {
            format,
            embeddings,
            response: ProviderResponse::default(),
            outcome: StreamOutcome::Open,
            error_name: None,
            pending: CutStrings::default(),
            skipping: false,
            after_cr: false,
        }
    }

    /// Read the next bytes of the stream.
    pub fn feed(&mut self, chunk: &[u8]) {
        let mut start = 0;
        let mut at = 0;
        if self.after_cr && chunk.first() == Some(&b'\n') {
            // The CRLF was split between two chunks.
            start = 1;
            at = 1;
        }
        self.after_cr = false;
        while let Some(n) = memchr::memchr2(b'\r', b'\n', &chunk[at..]) {
            at += n;
            self.end_line(&chunk[start..at]);
            if chunk[at] == b'\r' {
                match chunk.get(at + 1) {
                    Some(b'\n') => at += 1,
                    None => self.after_cr = true,
                    Some(_) => {}
                }
            }
            at += 1;
            start = at;
        }
        self.hold(&chunk[start..]);
    }

    fn hold(&mut self, rest: &[u8]) {
        if self.skipping || rest.is_empty() {
            return;
        }
        self.pending.push(rest);
        if self.pending.out.len() > MAX_STREAM_LINE_BYTES {
            self.pending.clear();
            self.skipping = true;
        }
    }

    fn end_line(&mut self, tail: &[u8]) {
        if self.skipping {
            self.skipping = false;
            self.pending.clear();
            return;
        }
        if self.pending.is_empty() && tail.len() <= CUT_ABOVE_BYTES {
            self.line(tail);
            return;
        }
        // Cut as a line held across chunks is, so the usage read does not
        // depend on where the transport split the stream.
        self.pending.push(tail);
        let mut pending = std::mem::take(&mut self.pending);
        if pending.out.len() <= MAX_STREAM_LINE_BYTES {
            self.line(&pending.out);
        }
        pending.clear();
        self.pending = pending;
    }

    fn line(&mut self, line: &[u8]) {
        let Ok(line) = std::str::from_utf8(line) else {
            return;
        };
        let line = line.trim();
        let (field, value) = match line.split_once(':') {
            Some((field @ ("data" | "event"), value)) => (field, value.trim()),
            _ => ("data", line),
        };
        if field == "event" {
            self.event_name(value);
        } else {
            self.data(value);
        }
    }

    /// Read a server-sent event that the caller already split: its `event`
    /// field (empty when it had none) and its `data`, as
    /// [`UsageStream::feed`] reads them from a stream, long strings cut
    /// alike.
    pub fn feed_event(&mut self, event_name: &str, data: &str) {
        let event_name = event_name.trim();
        if !event_name.is_empty() {
            self.event_name(event_name);
        }
        if data.len() <= CUT_ABOVE_BYTES {
            self.data(data.trim());
            return;
        }
        let cut = cut_strings(data.as_bytes());
        if cut.len() <= MAX_STREAM_LINE_BYTES
            && let Ok(data) = std::str::from_utf8(&cut)
        {
            self.data(data.trim());
        }
    }

    fn event_name(&mut self, name: &str) {
        match name {
            "message_stop" | "response.completed" | "response.incomplete" => {
                self.complete();
            }
            "response.failed" => self.fail(None),
            _ => {}
        }
    }

    fn data(&mut self, value: &str) {
        if value == "[DONE]" {
            // Only OpenAI-shaped streams end with `[DONE]`; Anthropic's and
            // Gemini's have terminal events of their own.
            if !matches!(self.format, UsageFormat::Anthropic | UsageFormat::Gemini) {
                self.complete();
            }
            return;
        }
        if !value.starts_with('{') && !value.starts_with('[') {
            return;
        }
        // A content chunk names the model and tier again and carries no
        // usage: once both are known it is skipped without being decoded.
        if !self.wanted(value) {
            return;
        }
        if let Ok(event) = Json::parse_pruned(value, EVENT) {
            self.event(&event);
        }
    }

    /// Whether a data line can carry what the stream reads, by one scan of
    /// its quotes that decodes nothing.
    fn wanted(&self, value: &str) -> bool {
        let bytes = value.as_bytes();
        memchr::memchr_iter(b'"', bytes).any(|at| {
            let Some(next) = bytes.get(at + 1) else {
                return false;
            };
            let rest = &value[at..];
            CANDIDATES.iter().any(|(name, kind)| {
                name.as_bytes()[1] == *next
                    && rest.starts_with(name)
                    && match kind {
                        Candidate::Marker => true,
                        Candidate::Member => not_null_member(&rest[name.len()..]),
                        Candidate::Model => self.response.response_model.is_none(),
                        Candidate::Tier => {
                            self.response.service_tier.is_none()
                                && not_null_member(&rest[name.len()..])
                        }
                    }
            })
        })
    }

    fn complete(&mut self) {
        if self.outcome == StreamOutcome::Open {
            self.outcome = StreamOutcome::Completed;
        }
    }

    fn fail(&mut self, name: Option<String>) {
        self.outcome = StreamOutcome::Failed;
        if self.error_name.is_none() {
            self.error_name = name;
        }
    }

    /// Read one decoded event, as [`UsageStream::feed`] does for each line.
    pub fn event(&mut self, event: &Json) {
        let kind = member(Some(event), "type").and_then(Json::as_str);
        match kind {
            Some("message_stop" | "response.completed" | "response.incomplete") => {
                self.complete();
            }
            Some("response.failed") => {
                let error = member(member(Some(event), "response"), "error");
                self.fail(error_name(error));
            }
            Some("error") => {
                // Anthropic nests the error; the Responses error event holds
                // its code beside the event type.
                let name = error_name(member(Some(event), "error"))
                    .or_else(|| text(member(Some(event), "code")));
                self.fail(name);
            }
            _ => {}
        }
        if let Some(error) = member(Some(event), "error") {
            let content = ["choices", "candidates"]
                .iter()
                .any(|key| member(Some(event), key).is_some());
            if kind != Some("error") && !content && error.as_object().is_some() {
                self.fail(error_name(Some(error)));
            }
        }
        if let Some(candidates) = member(Some(event), "candidates").and_then(Json::as_array)
            && candidates
                .iter()
                .any(|c| text(member(Some(c), "finishReason")).is_some())
        {
            self.complete();
        }
        self.response
            .merge(parse_provider_response(self.format, event, self.embeddings));
    }

    /// What the stream reported so far.
    pub fn response(&self) -> &ProviderResponse {
        &self.response
    }

    pub fn outcome(&self) -> StreamOutcome {
        self.outcome
    }

    /// The provider's name for the failure an error event reported (its
    /// `type`, `code` or `status`), to classify with
    /// [`crate::classify_error`].
    pub fn error_name(&self) -> Option<&str> {
        self.error_name.as_deref()
    }

    /// Read a final line that had no line ending, and return what the stream
    /// reported.
    pub fn finish(mut self) -> (ProviderResponse, StreamOutcome, Option<String>) {
        self.read_last_line();
        (self.response, self.outcome, self.error_name)
    }

    /// What [`UsageStream::finish`] would return, leaving the stream to be
    /// fed further. Only the report is copied, not the line being held,
    /// unless that line is the last one and must be read.
    pub fn report(&self) -> (ProviderResponse, StreamOutcome, Option<String>) {
        if !self.skipping && !self.pending.is_empty() {
            let mut last = UsageStream {
                format: self.format,
                embeddings: self.embeddings,
                response: self.response.clone(),
                outcome: self.outcome,
                error_name: self.error_name.clone(),
                pending: CutStrings::default(),
                skipping: false,
                after_cr: false,
            };
            last.line(&self.pending.out);
            return (last.response, last.outcome, last.error_name);
        }
        (self.response.clone(), self.outcome, self.error_name.clone())
    }

    fn read_last_line(&mut self) {
        if !self.skipping && !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending.out);
            self.line(&line);
        }
    }
}

/// The name an error object gives its failure: `type`, else `code`, else
/// `status`. A numeric code is read as its digits.
fn error_name(error: Option<&Json>) -> Option<String> {
    for key in ["type", "code", "status"] {
        match member(error, key) {
            Some(Json::String(name)) if !name.trim().is_empty() => {
                return Some(name.trim().to_string());
            }
            Some(number @ Json::Number(_)) => {
                if let Some(code) = count(Some(number)) {
                    return Some(code.to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Read a whole provider response body: a JSON document, or a stream of
/// server-sent events that was buffered.
pub fn parse_provider_body(
    format: UsageFormat,
    body: &[u8],
    embeddings: bool,
) -> (ProviderResponse, StreamOutcome, Option<String>) {
    let start = body
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(body.len());
    let body_text = &body[start..];
    // A large document is mostly generated text: cut its strings before
    // decoding (and before checking its UTF-8), so none of it is copied.
    let cut;
    let document = if body_text.len() > CUT_ABOVE_BYTES {
        cut = cut_strings(body_text);
        cut.as_slice()
    } else {
        body_text
    };
    if matches!(body_text.first(), Some(b'{' | b'['))
        && let Ok(text) = std::str::from_utf8(document)
        && let Ok(document) = Json::parse_pruned(text, EVENT)
    {
        let mut stream = UsageStream::new(format, embeddings);
        match document.as_array() {
            Some(events) => events.iter().for_each(|event| stream.event(event)),
            None => stream.event(&document),
        }
        let (response, outcome, error) = stream.finish();
        // A buffered body is complete unless it reported a failure.
        let outcome = match outcome {
            StreamOutcome::Open => StreamOutcome::Completed,
            outcome => outcome,
        };
        return (response, outcome, error);
    }
    let mut stream = UsageStream::new(format, embeddings);
    stream.feed(body);
    stream.finish()
}

/// The longest account, project or key id kept.
const MAX_ID_LENGTH: usize = 128;

/// The response headers that name the account and project a provider billed:
/// (`gen_ai.provider.name`, attribute, header). No other header is read.
pub const PROVIDER_ID_HEADERS: [(&str, &str, &str); 3] = [
    (
        "anthropic",
        "trajectory.request.provider_account_id",
        "anthropic-organization-id",
    ),
    // The organization's name, not its org- id.
    (
        "openai",
        "trajectory.request.provider_account_id",
        "openai-organization",
    ),
    ("openai", "trajectory.request.project_id", "openai-project"),
];

/// An account, project or key id as written: non-blank, at most 128 bytes.
/// Leading and trailing whitespace is trimmed on purpose: it is not part of
/// an HTTP field value (RFC 9110, section 5.5), and a proxy or client that
/// leaves it in would otherwise split one account into several series.
fn billing_id(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= MAX_ID_LENGTH).then_some(value)
}

/// The account and project the provider's response headers name, as
/// deployment attributes. `provider_name` is the `gen_ai.provider.name`, or
/// empty when the caller does not know it: then every header of
/// [`PROVIDER_ID_HEADERS`] is tried, in its order, and the first that names
/// an attribute sets it. No provider sends another's headers. `header`
/// returns a header's value by its lower-case name.
pub fn provider_id_attributes<'a>(
    provider_name: &str,
    header: impl Fn(&str) -> Option<&'a str>,
) -> Attributes {
    let mut attributes = Attributes::new();
    for (provider, attribute, name) in PROVIDER_ID_HEADERS {
        let applies = provider_name.is_empty() || provider == provider_name;
        if !applies || attributes.contains_key(attribute) {
            continue;
        }
        if let Some(value) = header(name).and_then(billing_id) {
            attributes.insert(attribute.to_string(), value.to_string());
        }
    }
    attributes
}

/// The AWS account a Bedrock model ARN names, such as an inference
/// profile's: the 12 digits of `arn:aws*:bedrock:{region}:{account}:...`,
/// at the start of `model` or after a `/`.
pub fn bedrock_arn_account(model: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(found) = model[from..].find("arn:aws") {
        let start = from + found;
        from = start + 1;
        if start > 0 && model.as_bytes()[start - 1] != b'/' {
            continue;
        }
        let mut parts = model[start..].splitn(6, ':');
        let (Some(_arn), Some(partition), Some(service), Some(region), Some(account), Some(_)) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            continue;
        };
        let partition_ok = partition
            .strip_prefix("aws")
            .is_some_and(|rest| rest.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'));
        let region_ok = !region.is_empty()
            && region
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        let account_ok = account.len() == 12 && account.bytes().all(|b| b.is_ascii_digit());
        if partition_ok && service == "bedrock" && region_ok && account_ok {
            return Some(account);
        }
    }
    None
}

/// Whether `value` names a person: an email address,
/// `^[^@\s]+@[^@\s]+\.[^@\s]+$`. `user.id` is only ever a person; a service
/// or a credential is named by `trajectory.gateway.key.alias` instead.
pub fn is_person_identifier(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    let plain =
        |part: &str| !part.is_empty() && !part.contains('@') && !part.contains(char::is_whitespace);
    plain(local)
        && plain(domain)
        && domain
            .char_indices()
            .any(|(at, c)| c == '.' && at > 0 && at + 1 < domain.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cut(text: &str) -> String {
        String::from_utf8(cut_strings(text.as_bytes())).unwrap()
    }

    #[test]
    #[cfg_attr(miri, ignore)] // many large inputs are too slow under Miri
    fn long_strings_are_cut_at_safe_boundaries() {
        let short = r#"{"model": "gpt-4o", "usage": {"prompt_tokens": 3}}"#;
        assert_eq!(cut(short), short);

        let n = MAX_KEPT_STRING_BYTES;
        for filler in ["a", "é", r"\u00e9", r"\\", r#"\""#, r"\ud83d\ude00", r"\n"] {
            for pad in 0..6 {
                let text = format!(
                    r#"{{"text": "{}{}", "usage": {{"output_tokens": 5}}}}"#,
                    "x".repeat(n - pad),
                    filler.repeat(2048)
                );
                let out = cut(&text);
                assert!(out.len() < n + 64, "{filler} {pad}");
                let value = Json::parse(&out).unwrap_or_else(|e| panic!("{filler} {pad}: {e}"));
                assert_eq!(
                    value
                        .get("usage")
                        .and_then(|u| u.get("output_tokens"))
                        .and_then(Json::as_f64),
                    Some(5.0)
                );
            }
        }
        // Multi-byte characters are never split.
        let text = format!(r#"["{}"]"#, "é".repeat(4096));
        let out = cut(&text);
        assert!(Json::parse(&out).is_ok());

        // Fed in pieces that end anywhere, the result is the same.
        let text = format!(
            r#"{{"a": "{}\ud83d\ude00", "b": "{}", "usage": {{"x": 1}}}}"#,
            "y".repeat(n - 3),
            "z\\\"".repeat(1000)
        );
        let whole = cut_strings(text.as_bytes());
        for step in [1, 2, 3, 7, 1000] {
            let mut pieces = CutStrings::default();
            for piece in text.as_bytes().chunks(step) {
                pieces.push(piece);
            }
            assert_eq!(pieces.out, whole, "{step}");
        }
    }

    /// The closing quote found one byte at a time.
    fn closing_quote_bytewise(bytes: &[u8]) -> Result<usize, bool> {
        let mut escaped = false;
        for (i, &b) in bytes.iter().enumerate() {
            if escaped {
                escaped = false;
            } else if b == b'"' {
                return Ok(i);
            } else if b == b'\\' {
                escaped = true;
            }
        }
        Err(escaped)
    }

    #[test]
    #[cfg_attr(miri, ignore)] // many inputs are too slow under Miri
    fn the_block_scanner_finds_the_quote_a_bytewise_scan_finds() {
        // Runs of backslashes of every length, across 64-byte block edges.
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let alphabet = [b'a', b'"', b'\\', b'\\', b'\\', b'n', 0xC3, 0xA9];
        for len in [0, 1, 2, 63, 64, 65, 127, 128, 129, 200, 1000] {
            for _ in 0..300 {
                let text: Vec<u8> = (0..len)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        alphabet[(seed % alphabet.len() as u64) as usize]
                    })
                    .collect();
                assert_eq!(
                    closing_quote(&text),
                    closing_quote_bytewise(&text),
                    "{:?}",
                    String::from_utf8_lossy(&text)
                );
            }
        }
        let run = |n: usize| format!("{}{}\"", "x".repeat(70), "\\".repeat(n));
        assert_eq!(closing_quote(run(64).as_bytes()), Ok(134));
        assert_eq!(closing_quote(run(65).as_bytes()), Err(false));
        assert_eq!(closing_quote(b"abc\\"), Err(true));
    }

    #[test]
    fn a_pruned_event_reads_as_the_whole_event() {
        let events = [
            r#"{"type":"response.completed","response":{"model":"gpt-4.1","output":[{"content":[{"text":"hi \"there\""}]}],"service_tier":"default","usage":{"input_tokens":12,"output_tokens":18,"output_tokens_details":{"reasoning_tokens":3}},"error":null}}"#,
            r#"{"model":"gemini","candidates":[{"content":{"parts":[{"text":"x"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":4,"totalTokenCount":7}}"#,
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
            r#"{"choices":[{"delta":{"content":"x"}}],"error":{"code":500,"message":"boom"}}"#,
            r#"{"prompt_tokens":4,"completion_tokens":2,"total_tokens":6}"#,
            r#"[{"usageMetadata":{"promptTokenCount":1}},{"candidates":[{"finishReason":"STOP"}]}]"#,
        ];
        for text in events {
            let whole = Json::parse(text).unwrap();
            let pruned = Json::parse_pruned(text, EVENT).unwrap();
            for format in UsageFormat::ALL {
                assert_eq!(
                    parse_provider_response(format, &pruned, false),
                    parse_provider_response(format, &whole, false),
                    "{text}"
                );
            }
            let mut a = UsageStream::new(UsageFormat::Auto, false);
            a.event(&whole);
            let mut b = UsageStream::new(UsageFormat::Auto, false);
            b.event(&pruned);
            assert_eq!(a.finish(), b.finish(), "{text}");
        }
        // Skipped members are still checked: invalid JSON is refused alike.
        for text in [r#"{"x": "a\qb"}"#, "{\"x\": \"a\u{1}b\"}", r#"{"x": [1,]}"#] {
            assert!(Json::parse_pruned(text, EVENT).is_err(), "{text}");
            assert!(Json::parse(text).is_err(), "{text}");
        }
    }
}
