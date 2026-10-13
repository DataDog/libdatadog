// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Agentless APM trace export.

mod config;
mod exporter;
#[cfg(feature = "stats-obfuscation")]
mod v04;

pub use config::{AgentlessTraceConfig, DEFAULT_AGENTLESS_TIMEOUT};
pub use exporter::{AgentlessError, send_agentless_traces, send_agentless_traces_with_observer};
#[cfg(feature = "stats-obfuscation")]
pub use v04::{
    AgentlessStatsConfig, AgentlessV04Error, AgentlessV04Exporter, agentless_stats_version,
};
