// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![deny(clippy::panic)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]

//! Runtime-independent APM data-pipeline operations.
//!
//! Enable `stats-obfuscation` to use the agentless v0.4 stats exporter.

mod agentless;

#[cfg(feature = "stats-obfuscation")]
pub use agentless::{
    agentless_stats_version, AgentlessStatsConfig, AgentlessV04Error, AgentlessV04Exporter,
};
pub use agentless::{
    send_agentless_traces, send_agentless_traces_with_observer, AgentlessError,
    AgentlessTraceConfig, DEFAULT_AGENTLESS_TIMEOUT,
};
pub use libdd_trace_utils::tracer_metadata::TracerMetadata;
