// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Usage and cost metric points for LLM provider calls and gateway requests.
//!
//! An integration describes one call as a plain observation. This crate checks
//! it, normalizes its token usage to the OpenTelemetry GenAI definitions, and
//! returns the metric points of the profile, with any issue codes raised beside
//! them. It performs no I/O, keeps no state and reads no environment.
//!
//! The rules are those of the Open Trajectory portable metric profiles. This
//! crate is a port of their Rust reference implementation, and its tests run
//! the shared conformance cases in `tests/data`.
//!
//! Profiles implemented here:
//!
//! - [`Profile::ProviderAttempt`] (`gen_ai.client.provider_attempt@0.1.0`): one request to a model
//!   provider. Duration, token counts and cost.
//! - [`Profile::TokenBreakdown`] (`trajectory.gen_ai.client.token_breakdown@0.1.0`): the cache and
//!   reasoning parts of the same request.
//! - [`Profile::ProviderStreaming`] (`gen_ai.client.provider_streaming@0.1.0`): the chunk intervals
//!   of a streamed request.
//! - [`Profile::GatewayRequest`] (`trajectory.gen_ai.gateway.request@0.1.0`): one client request to
//!   a gateway, which can span several provider requests.
//!
//! A projection returns the points, the issue codes raised beside them, and any
//! resource attributes; a rejected input returns a [`MetricError`] with a
//! stable [`ErrorCode`].
//!
//! Beside the profiles, the crate reads what a provider reported, the way
//! every integration needs it: the usage of a response or stream
//! ([`parse_provider_response`], [`UsageStream`]), its `gen_ai.provider.name`
//! ([`gen_ai_provider_name`]), its `error.type` ([`classify_error`]), and the
//! account and project it billed ([`provider_id_attributes`]). It also packs
//! DogStatsD lines into datagrams ([`pack_dogstatsd`]).
//!
//! ```
//! use libdd_ai_usage::{Json, Profile, project_result};
//!
//! let observation = Json::parse(
//!     r#"{
//!         "operation_name": "chat",
//!         "provider_name": "openai",
//!         "duration_seconds": 0.42,
//!         "streaming": false,
//!         "input_tokens": 12,
//!         "output_tokens": 2,
//!         "observation_point": "gateway"
//!     }"#,
//! )?;
//! let projection = project_result(Profile::ProviderAttempt, &observation)?;
//! // Duration, plus a per-operation histogram and a usage counter per direction.
//! assert_eq!(projection.points.len(), 5);
//! assert!(projection.issues.is_empty());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

// Items not exported from the crate root stay crate-private.
#![warn(unreachable_pub)]

mod batch;
mod datadog;
mod deployment;
mod dogstatsd;
mod error_type;
mod gateway;
mod intake;
mod json;
mod number;
mod observation;
#[cfg(feature = "otlp")]
mod otlp;
mod point;
mod project;
mod provider;
// The metric registry (`tests/data/registry.json`), read only by the tests
// that check the crate against it.
#[cfg(test)]
mod registry;
#[cfg(test)]
extern crate self as libdd_ai_usage;
#[cfg(test)]
mod conformance_tests;
#[cfg(test)]
mod robustness_tests;
mod usage;

pub use batch::MetricBatch;
pub use datadog::{DatadogSeries, SeriesType, datadog_series, datadog_tag_value};
pub use deployment::{
    Carrier, DEPLOYMENT_ATTRIBUTES, deployment_attribute_carrier, is_deployment_value,
    with_deployment_attributes, with_deployment_attributes_json,
};
pub use dogstatsd::{
    DOGSTATSD_MAX_LINE_BYTES, DOGSTATSD_UDP_MAX_PAYLOAD, DOGSTATSD_UDS_MAX_PAYLOAD, pack_dogstatsd,
    pack_dogstatsd_counted, pack_dogstatsd_joined, with_constant_tags,
};
pub use error_type::{
    ERROR_TYPE_API, ERROR_TYPE_API_CONNECTION, ERROR_TYPE_AUTHENTICATION, ERROR_TYPE_BAD_REQUEST,
    ERROR_TYPE_CANCELLED, ERROR_TYPE_CONFLICT, ERROR_TYPE_INTERNAL_SERVER, ERROR_TYPE_NOT_FOUND,
    ERROR_TYPE_PERMISSION_DENIED, ERROR_TYPE_RATE_LIMIT, ERROR_TYPE_TIMEOUT,
    ERROR_TYPE_UNPROCESSABLE_ENTITY, ERROR_TYPES, classify_error, classify_known_error,
    error_type_for_name, error_type_for_status, error_type_identifier,
};
pub use gateway::operation_name_for;
pub use intake::{IntakeTake, RateSeries, Sketch, SketchSeries};
pub use json::{Json, JsonError, JsonErrorKind, JsonObject};
pub use number::{MAX_SAFE_INTEGER, nano_usd_from_number, nano_usd_from_usd, usd_from_nano_usd};
pub use observation::{
    GatewayRequestObservation, OBSERVATION_POINTS, ProviderAttemptObservation,
    ProviderStreamingObservation, TOKEN_SOURCES, TokenBreakdownObservation, is_context_band,
    is_identifier, is_rate_card,
};
#[cfg(feature = "otlp")]
pub use otlp::{OtlpBinding, OtlpScope, OtlpWindow};
pub use point::{
    Attributes, ErrorCode, Instrument, IssueCode, Issues, MetricError, MetricPoint, Projection,
    compare_metric_points, merge_counter_points, sort_metric_points,
};
pub use project::{
    project_gateway_request, project_provider_attempt, project_provider_streaming,
    project_token_breakdown,
};
pub use provider::{
    MAX_ERROR_MESSAGE_BYTES, MAX_KEPT_STRING_BYTES, MAX_STREAM_LINE_BYTES, PROVIDER_ID_HEADERS,
    ProviderError, ProviderResponse, ProviderUsage, RESPONSE_MODEL_DROPPED, SERVICE_TIER_DROPPED,
    StreamOutcome, UsageFormat, UsageStream, bedrock_arn_account, gen_ai_provider_name,
    is_person_identifier, is_service_tier, parse_provider_body, parse_provider_response,
    parse_provider_usage, provider_id_attributes, vertex_project_id,
};
/// The attribute that carries the profile identifier: on the OTLP scope, or
/// on each point when the SDK cannot set scope attributes.
pub const PROFILE_ATTRIBUTE: &str = "trajectory.profile";
pub use usage::{
    InputBasis, Modality, NormalizedUsage, OutputBasis, ReportedUsage, normalize_usage,
};

/// A metric profile implemented by this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Profile {
    ProviderAttempt,
    ProviderStreaming,
    TokenBreakdown,
    GatewayRequest,
}

impl Profile {
    pub const ALL: [Profile; 4] = [
        Profile::ProviderAttempt,
        Profile::ProviderStreaming,
        Profile::TokenBreakdown,
        Profile::GatewayRequest,
    ];

    /// The versioned profile identifier, `{id}@{version}`. It is the value
    /// of the `trajectory.profile` scope attribute of the profile's points.
    pub fn id(self) -> &'static str {
        match self {
            Profile::ProviderAttempt => "gen_ai.client.provider_attempt@0.1.0",
            Profile::ProviderStreaming => "gen_ai.client.provider_streaming@0.1.0",
            Profile::TokenBreakdown => "trajectory.gen_ai.client.token_breakdown@0.1.0",
            Profile::GatewayRequest => "trajectory.gen_ai.gateway.request@0.1.0",
        }
    }

    /// The metrics the profile defines, as the registry lists them.
    pub fn metric_names(self) -> &'static [&'static str] {
        match self {
            Profile::ProviderAttempt => &[
                "gen_ai.client.inference.duration",
                "gen_ai.client.inference.operation.input_tokens",
                "gen_ai.client.inference.operation.output_tokens",
                "gen_ai.client.inference.usage.input_tokens",
                "gen_ai.client.inference.usage.output_tokens",
                "gen_ai.client.inference.time_to_first_chunk",
                "gen_ai.client.operation.duration",
                "trajectory.gen_ai.client.inference.usage.server_tool.requests",
                "trajectory.gen_ai.client.inference.usage.cost",
                "trajectory.gen_ai.client.operation.cost",
            ],
            Profile::ProviderStreaming => &["gen_ai.client.inference.time_per_output_chunk"],
            Profile::TokenBreakdown => &[
                "gen_ai.client.inference.usage.cache_read.input_tokens",
                "gen_ai.client.inference.usage.cache_write.input_tokens",
                "gen_ai.client.inference.usage.reasoning.output_tokens",
                "trajectory.gen_ai.client.inference.usage.uncached.input_tokens",
            ],
            Profile::GatewayRequest => &[
                "trajectory.gen_ai.gateway.request.duration",
                "trajectory.gen_ai.gateway.request.provider_operations",
                "trajectory.gen_ai.gateway.request.retries",
                "trajectory.gen_ai.gateway.request.fallbacks",
                "trajectory.gen_ai.gateway.cache.operations",
                "trajectory.gen_ai.gateway.request.estimated_cost",
            ],
        }
    }

    /// The fixture key that holds this profile's input.
    pub fn input_key(self) -> &'static str {
        "observation"
    }

    pub fn from_id(id: &str) -> Option<Profile> {
        Profile::ALL.into_iter().find(|p| p.id() == id)
    }
}

/// Project `input`, an observation object, under `profile`.
pub fn project_result(profile: Profile, input: &Json) -> Result<Projection, MetricError> {
    match profile {
        Profile::ProviderAttempt => {
            project_provider_attempt(&ProviderAttemptObservation::from_json(input)?)
        }
        Profile::ProviderStreaming => {
            project_provider_streaming(&ProviderStreamingObservation::from_json(input)?)
        }
        Profile::TokenBreakdown => {
            project_token_breakdown(&TokenBreakdownObservation::from_json(input)?)
        }
        Profile::GatewayRequest => {
            project_gateway_request(&GatewayRequestObservation::from_json(input)?)
        }
    }
}

/// Keep only the points of the metrics named in `metrics`, as an exporter
/// that sends a subset of a profile's metrics does. A Histogram's Datadog
/// companions (`{name}.sum`, `{name}.count`) follow their metric.
pub fn retain_metrics<S: AsRef<str>>(projection: &mut Projection, metrics: &[S]) {
    projection
        .points
        .retain(|point| metrics.iter().any(|name| name.as_ref() == point.name));
}

/// Project a conformance fixture document: `profile`, the profile's input
/// key, and optional `deployment_attributes`, which are applied after
/// projection.
///
/// A fixture that is not shaped like one (no profile, a profile this crate
/// does not implement, no input) is not a rejection of the contract; it
/// reports `profile_invalid` or `required_field_missing` so that a harness
/// sees it fail.
pub fn project_fixture_result(fixture: &Json) -> Result<Projection, MetricError> {
    let missing = |what: &str| MetricError::new(ErrorCode::RequiredFieldMissing, what);
    let id = fixture
        .get("profile")
        .and_then(Json::as_str)
        .ok_or_else(|| missing("the fixture has no profile"))?;
    let profile = Profile::from_id(id).ok_or_else(|| {
        MetricError::new(
            ErrorCode::ProfileInvalid,
            format!("unsupported profile: {id}"),
        )
    })?;
    // An absent input is a missing one, which the projector reports.
    let input = fixture.get(profile.input_key()).unwrap_or(&Json::Null);
    let projection = project_result(profile, input)?;
    let attributes = fixture.get("deployment_attributes").unwrap_or(&Json::Null);
    with_deployment_attributes_json(projection, attributes)
}
