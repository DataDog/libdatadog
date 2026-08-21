// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![deny(clippy::panic)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]

//! Runtime-independent preparation of APM data-pipeline requests.

mod agentless;

pub use agentless::{
    prepare_agentless_json_request, prepare_agentless_traces_request,
    prepare_agentless_v04_request, AgentlessTraceConfig, PrepareAgentlessError,
    PreparedAgentlessRequest, DEFAULT_AGENTLESS_TIMEOUT,
};
pub use libdd_trace_utils::tracer_metadata::TracerMetadata;
