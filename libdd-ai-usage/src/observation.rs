// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Normalized observations accepted by the observation-based profiles.
//!
//! Reading an observation from JSON makes only the first check of
//! `PORTABLE-METRICS.md#order-of-checks`: a field of the wrong JSON type is
//! `field_type_invalid`. A JSON `null` reads as absent. Every later check is
//! made by the projector, on the typed observation, in the contract's order,
//! so a caller that builds an observation directly gets the same checks.
//!
//! Numbers are `f64`. An infinity stands for a JSON number no double holds.

use std::collections::BTreeMap;

use crate::json::{Json, JsonObject};
use crate::point::{ErrorCode, MetricError, reject};
use crate::usage::ReportedUsage;

/// One resolved provider client operation
/// (`gen_ai.client.provider_attempt@0.1.0`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderAttemptObservation {
    pub operation_name: Option<String>,
    pub provider_name: Option<String>,
    pub request_model: Option<String>,
    pub response_model: Option<String>,
    pub duration_seconds: Option<f64>,
    pub streaming: Option<bool>,
    pub time_to_first_chunk_seconds: Option<f64>,
    pub usage: ReportedUsage,
    pub cost_usd: Option<f64>,
    pub cost_nano_usd: Option<f64>,
    pub cost_source: Option<String>,
    pub cost_rate_card: Option<String>,
    pub cost_bound: Option<String>,
    /// The model's canonical name as the pricing contract resolved it
    /// (`PORTABLE-METRICS.md#canonical-model`). Only the cost points carry
    /// it; every point keeps the names as received.
    pub canonical_provider: Option<String>,
    pub canonical_model: Option<String>,
    pub model_family: Option<String>,
    /// The model whose prices were used when a fallback priced another model
    /// than the one served. Only an estimated cost has one.
    pub cost_priced_as_model: Option<String>,
    pub context_band: Option<String>,
    /// The tier the provider applied, in the provider's own words. The token
    /// and cost Counters carry it as `trajectory.request.service_tier`.
    pub service_tier: Option<String>,
    /// `true` when the provider served the operation as a batch request. The
    /// token and cost Counters then carry `trajectory.request.batch`.
    pub batch: Option<bool>,
    pub error_type: Option<String>,
    /// Where the operation was observed, one of [`OBSERVATION_POINTS`]. When
    /// stated, every point carries `trajectory.observation.point`.
    pub observation_point: Option<String>,
    /// Whether each side's counts were reported or estimated, one of
    /// [`TOKEN_SOURCES`]. Absent, like `reported`, means reported; an
    /// estimated side marks its token points with `trajectory.token.source`.
    pub input_token_source: Option<String>,
    pub output_token_source: Option<String>,
}

/// Output-chunk intervals of one streamed operation
/// (`gen_ai.client.provider_streaming@0.1.0`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderStreamingObservation {
    pub operation_name: Option<String>,
    pub provider_name: Option<String>,
    pub request_model: Option<String>,
    pub response_model: Option<String>,
    pub streaming: Option<bool>,
    pub output_chunk_intervals_seconds: Option<Vec<f64>>,
    /// Where the operation was observed, one of [`OBSERVATION_POINTS`]. When
    /// stated, every point carries `trajectory.observation.point`.
    pub observation_point: Option<String>,
}

/// Terminal token usage of one provider operation with its optional cache and
/// reasoning parts (`trajectory.gen_ai.client.token_breakdown@0.1.0`). The
/// modality and server-tool members of `usage` are not part of this profile
/// and are left unset when reading.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TokenBreakdownObservation {
    pub operation_name: Option<String>,
    pub provider_name: Option<String>,
    pub request_model: Option<String>,
    pub response_model: Option<String>,
    pub usage: ReportedUsage,
    pub context_band: Option<String>,
    /// The tier the provider applied, in the provider's own words. Every
    /// breakdown Counter carries it as `trajectory.request.service_tier`.
    pub service_tier: Option<String>,
    /// `true` when the provider served the operation as a batch request.
    /// Every breakdown Counter then carries `trajectory.request.batch`.
    pub batch: Option<bool>,
    /// Where the operation was observed, one of [`OBSERVATION_POINTS`]. When
    /// stated, every point carries `trajectory.observation.point`.
    pub observation_point: Option<String>,
    /// Whether each side's counts were reported or estimated, one of
    /// [`TOKEN_SOURCES`]. Absent, like `reported`, means reported; an
    /// estimated side marks its token points with `trajectory.token.source`.
    pub input_token_source: Option<String>,
    pub output_token_source: Option<String>,
}

/// One logical gateway request (`trajectory.gen_ai.gateway.request@0.1.0`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GatewayRequestObservation {
    pub operation_name: Option<String>,
    pub request_model: Option<String>,
    pub duration_seconds: Option<f64>,
    pub provider_operations: Option<f64>,
    pub provider_operation_coverage: Option<String>,
    pub retries: Option<f64>,
    pub fallbacks: Option<f64>,
    pub cache_outcomes: Option<Vec<String>>,
    /// The gateway's own cost figure for the whole request, in USD and in
    /// integer nano-USD. Either or both may be stated.
    pub estimated_cost_usd: Option<f64>,
    pub estimated_cost_nano_usd: Option<f64>,
    pub error_type: Option<String>,
    /// May state `gateway`, the only point that observes a logical gateway
    /// request. Every point carries `gateway` whether or not it is stated.
    pub observation_point: Option<String>,
}

/// The values of `input_token_source` and `output_token_source`
/// (`PORTABLE-METRICS.md#token-source`).
pub const TOKEN_SOURCES: [&str; 2] = ["reported", "estimated"];

/// The closed list of `OBSERVATION-POINTS.md#the-observation-points`.
pub const OBSERVATION_POINTS: [&str; 7] = [
    "harness",
    "harness_transcript",
    "agent_framework",
    "gateway",
    "network_proxy",
    "provider",
    "telemetry_import",
];

/// `^[a-z][a-z0-9_.-]*$`: a low-cardinality identifier.
pub fn is_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'))
}

/// `^[A-Za-z0-9._-]{1,32}$`: a context band value.
pub fn is_context_band(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A rate card as `{id}@{version}`, each side in the identifier grammar the
/// pricing contract states for a card (SPEC-PRICING.md Section 3.1):
/// `^[a-z0-9][a-z0-9._-]{0,63}$`.
pub fn is_rate_card(value: &str) -> bool {
    let plain = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    let side = |part: &str| {
        let bytes = part.as_bytes();
        (1..=64).contains(&bytes.len())
            && plain(bytes[0])
            && bytes[1..]
                .iter()
                .all(|&b| plain(b) || matches!(b, b'.' | b'_' | b'-'))
    };
    value
        .split_once('@')
        .is_some_and(|(id, version)| side(id) && side(version))
}

/// Typed access to the members of an observation object. A `null` member is
/// absent; a member of another type than asked for is `field_type_invalid`.
struct Fields<'a>(&'a JsonObject);

impl<'a> Fields<'a> {
    fn of(observation: &'a Json) -> Result<Self, MetricError> {
        match observation {
            Json::Object(map) => Ok(Self(map)),
            Json::Null => reject(
                ErrorCode::RequiredFieldMissing,
                "the observation is required",
            ),
            _ => reject(
                ErrorCode::FieldTypeInvalid,
                "the observation must be an object",
            ),
        }
    }

    fn present(&self, key: &str) -> Option<&'a Json> {
        self.0.get(key).filter(|value| !value.is_null())
    }

    fn wrong<T>(key: &str, expected: &str) -> Result<T, MetricError> {
        reject(
            ErrorCode::FieldTypeInvalid,
            format!("{key} must be {expected}"),
        )
    }

    fn number(&self, key: &str) -> Result<Option<f64>, MetricError> {
        match self.present(key) {
            None => Ok(None),
            Some(Json::Number(number)) => Ok(Some(*number)),
            Some(_) => Self::wrong(key, "a number"),
        }
    }

    fn string(&self, key: &str) -> Result<Option<String>, MetricError> {
        match self.present(key) {
            None => Ok(None),
            Some(Json::String(text)) => Ok(Some(text.clone())),
            Some(_) => Self::wrong(key, "a string"),
        }
    }

    fn boolean(&self, key: &str) -> Result<Option<bool>, MetricError> {
        match self.present(key) {
            None => Ok(None),
            Some(Json::Bool(flag)) => Ok(Some(*flag)),
            Some(_) => Self::wrong(key, "a boolean"),
        }
    }

    fn numbers(&self, key: &str) -> Result<Option<Vec<f64>>, MetricError> {
        match self.present(key) {
            None => Ok(None),
            Some(Json::Array(items)) => items
                .iter()
                .map(|item| item.as_f64())
                .collect::<Option<Vec<f64>>>()
                .map_or_else(
                    || Self::wrong(key, "an array of numbers"),
                    |list| Ok(Some(list)),
                ),
            Some(_) => Self::wrong(key, "an array of numbers"),
        }
    }

    fn strings(&self, key: &str) -> Result<Option<Vec<String>>, MetricError> {
        match self.present(key) {
            None => Ok(None),
            Some(Json::Array(items)) => items
                .iter()
                .map(|item| item.as_str().map(str::to_string))
                .collect::<Option<Vec<String>>>()
                .map_or_else(
                    || Self::wrong(key, "an array of strings"),
                    |list| Ok(Some(list)),
                ),
            Some(_) => Self::wrong(key, "an array of strings"),
        }
    }

    /// Parts by modality: an object whose members are numbers or `null`.
    /// Every member is type-checked, whatever its key.
    fn parts(&self, key: &str) -> Result<Option<BTreeMap<String, f64>>, MetricError> {
        let Some(value) = self.present(key) else {
            return Ok(None);
        };
        let Json::Object(members) = value else {
            return Self::wrong(key, "an object");
        };
        let mut parts = BTreeMap::new();
        for (name, part) in members {
            match part {
                Json::Null => {}
                Json::Number(number) => {
                    parts.insert(name.clone(), *number);
                }
                _ => return Self::wrong(key, "an object of numbers"),
            }
        }
        Ok(Some(parts))
    }

    /// The usage members both usage profiles share.
    fn usage(&self) -> Result<ReportedUsage, MetricError> {
        Ok(ReportedUsage {
            input_tokens: self.number("input_tokens")?,
            output_tokens: self.number("output_tokens")?,
            cache_read_input_tokens: self.number("cache_read_input_tokens")?,
            cache_write_input_tokens: self.number("cache_write_input_tokens")?,
            cache_write_5m_input_tokens: self.number("cache_write_5m_input_tokens")?,
            cache_write_1h_input_tokens: self.number("cache_write_1h_input_tokens")?,
            reasoning_output_tokens: self.number("reasoning_output_tokens")?,
            input_basis: self.string("input_basis")?,
            output_basis: self.string("output_basis")?,
            ..ReportedUsage::default()
        })
    }
}

impl ProviderAttemptObservation {
    /// Read an observation, checking JSON types only.
    pub fn from_json(observation: &Json) -> Result<Self, MetricError> {
        let fields = Fields::of(observation)?;
        Ok(Self {
            operation_name: fields.string("operation_name")?,
            provider_name: fields.string("provider_name")?,
            request_model: fields.string("request_model")?,
            response_model: fields.string("response_model")?,
            duration_seconds: fields.number("duration_seconds")?,
            streaming: fields.boolean("streaming")?,
            time_to_first_chunk_seconds: fields.number("time_to_first_chunk_seconds")?,
            usage: ReportedUsage {
                web_search_requests: fields.number("web_search_requests")?,
                web_search_requests_secondary: fields.number("web_search_requests_secondary")?,
                input_tokens_by_modality: fields.parts("input_tokens_by_modality")?,
                output_tokens_by_modality: fields.parts("output_tokens_by_modality")?,
                ..fields.usage()?
            },
            cost_usd: fields.number("cost_usd")?,
            cost_nano_usd: fields.number("cost_nano_usd")?,
            cost_source: fields.string("cost_source")?,
            cost_rate_card: fields.string("cost_rate_card")?,
            cost_bound: fields.string("cost_bound")?,
            canonical_provider: fields.string("canonical_provider")?,
            canonical_model: fields.string("canonical_model")?,
            model_family: fields.string("model_family")?,
            cost_priced_as_model: fields.string("cost_priced_as_model")?,
            context_band: fields.string("context_band")?,
            service_tier: fields.string("service_tier")?,
            batch: fields.boolean("batch")?,
            error_type: fields.string("error_type")?,
            observation_point: fields.string("observation_point")?,
            input_token_source: fields.string("input_token_source")?,
            output_token_source: fields.string("output_token_source")?,
        })
    }
}

impl ProviderStreamingObservation {
    /// Read an observation, checking JSON types only.
    pub fn from_json(observation: &Json) -> Result<Self, MetricError> {
        let fields = Fields::of(observation)?;
        Ok(Self {
            operation_name: fields.string("operation_name")?,
            provider_name: fields.string("provider_name")?,
            request_model: fields.string("request_model")?,
            response_model: fields.string("response_model")?,
            streaming: fields.boolean("streaming")?,
            output_chunk_intervals_seconds: fields.numbers("output_chunk_intervals_seconds")?,
            observation_point: fields.string("observation_point")?,
        })
    }
}

impl TokenBreakdownObservation {
    /// Read an observation, checking JSON types only.
    pub fn from_json(observation: &Json) -> Result<Self, MetricError> {
        let fields = Fields::of(observation)?;
        Ok(Self {
            operation_name: fields.string("operation_name")?,
            provider_name: fields.string("provider_name")?,
            request_model: fields.string("request_model")?,
            response_model: fields.string("response_model")?,
            usage: fields.usage()?,
            context_band: fields.string("context_band")?,
            service_tier: fields.string("service_tier")?,
            batch: fields.boolean("batch")?,
            observation_point: fields.string("observation_point")?,
            input_token_source: fields.string("input_token_source")?,
            output_token_source: fields.string("output_token_source")?,
        })
    }
}

impl GatewayRequestObservation {
    /// Read an observation, checking JSON types only.
    pub fn from_json(observation: &Json) -> Result<Self, MetricError> {
        let fields = Fields::of(observation)?;
        Ok(Self {
            operation_name: fields.string("operation_name")?,
            request_model: fields.string("request_model")?,
            duration_seconds: fields.number("duration_seconds")?,
            provider_operations: fields.number("provider_operations")?,
            provider_operation_coverage: fields.string("provider_operation_coverage")?,
            retries: fields.number("retries")?,
            fallbacks: fields.number("fallbacks")?,
            cache_outcomes: fields.strings("cache_outcomes")?,
            estimated_cost_usd: fields.number("estimated_cost_usd")?,
            estimated_cost_nano_usd: fields.number("estimated_cost_nano_usd")?,
            error_type: fields.string("error_type")?,
            observation_point: fields.string("observation_point")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammars_match_the_contract() {
        assert!(is_identifier("aws.bedrock"));
        assert!(!is_identifier("Example AI"));
        assert!(!is_identifier(""));
        assert!(is_context_band("200k-1M"));
        assert!(!is_context_band("200k to 1M"));
        assert!(!is_context_band(""));
        assert!(!is_context_band(&"a".repeat(33)));
        assert!(is_rate_card("synthetic-basic@1"));
        assert!(is_rate_card("a.b_c-d@2026-10-01"));
        assert!(is_rate_card("0@0"));
        assert!(is_rate_card(&format!(
            "{}@{}",
            "i".repeat(64),
            "v".repeat(64)
        )));
        for bad in [
            "synthetic-basic",
            "a@b@c",
            "bad card@1",
            "@1",
            "a@",
            "caf\u{e9}@1",
            "a\u{7f}@1",
            // The identifier grammar of the pricing contract.
            "Synthetic-Basic@1",
            "synthetic-basic@V1",
            "synthetic/basic@1",
            "a:b@1",
            "-a@1",
            "a@.1",
        ] {
            assert!(!is_rate_card(bad), "{bad}");
        }
        assert!(!is_rate_card(&format!("{}@1", "i".repeat(65))));
    }

    #[test]
    fn a_boolean_is_not_a_number_and_null_is_absent() {
        let read = |text: &str| ProviderAttemptObservation::from_json(&Json::parse(text).unwrap());
        for text in [
            r#"{"input_tokens": true}"#,
            r#"{"input_tokens": "12"}"#,
            r#"{"input_tokens": [12]}"#,
            r#"{"streaming": 1}"#,
            r#"{"operation_name": 5}"#,
            r#"{"input_tokens_by_modality": [1]}"#,
            r#"{"input_tokens_by_modality": {"video": "x"}}"#,
            r#"[]"#,
        ] {
            assert_eq!(
                read(text).unwrap_err().code(),
                ErrorCode::FieldTypeInvalid,
                "{text}"
            );
        }
        let observation = read(
            r#"{"input_tokens": null, "streaming": null, "operation_name": null,
                "input_tokens_by_modality": {"text": null}, "cost_usd": 1e400}"#,
        )
        .unwrap();
        assert_eq!(observation.usage.input_tokens, None);
        assert_eq!(observation.streaming, None);
        assert_eq!(observation.operation_name, None);
        assert_eq!(
            observation.usage.input_tokens_by_modality,
            Some(BTreeMap::new())
        );
        assert_eq!(observation.cost_usd, Some(f64::INFINITY));
    }
}
