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

mod deployment;
mod json;
mod number;
mod observation;
mod point;
mod project;
mod registry;
mod usage;

pub use deployment::{
    Carrier, DEPLOYMENT_ATTRIBUTES, deployment_attribute_carrier, is_deployment_value,
    with_deployment_attributes, with_deployment_attributes_json,
};
pub use json::{Json, JsonError, JsonErrorKind, JsonObject};
pub use number::{MAX_SAFE_INTEGER, nano_usd_from_number, nano_usd_from_usd, usd_from_nano_usd};
pub use observation::{
    GatewayRequestObservation, OBSERVATION_POINTS, ProviderAttemptObservation,
    ProviderStreamingObservation, TOKEN_SOURCES, TokenBreakdownObservation, is_context_band,
    is_identifier, is_rate_card,
};
pub use point::{
    Attributes, ErrorCode, Instrument, IssueCode, Issues, MetricError, MetricPoint, Projection,
    compare_metric_points, merge_counter_points, sort_metric_points,
};
pub use project::{
    project_gateway_request, project_provider_attempt, project_provider_streaming,
    project_token_breakdown,
};
pub use registry::{
    AttributeDefinition, DeploymentAttributeDefinition, Finding, FindingCode, MetricDefinition,
    MetricOrigin, PROFILE_ATTRIBUTE, ProfileDefinition, ProfileIdentifier, Registry, parse_profile,
};
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
