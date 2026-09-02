// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Agentless APM trace export.

mod config;
mod exporter;

pub use config::{AgentlessTraceConfig, DEFAULT_AGENTLESS_TIMEOUT};
pub use exporter::{send_agentless_traces, send_agentless_traces_with_observer, AgentlessError};
