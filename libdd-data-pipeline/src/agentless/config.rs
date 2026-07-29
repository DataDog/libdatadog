// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Agentless APM trace export configuration.

use std::{fmt::Debug, time::Duration};

pub const DEFAULT_AGENTLESS_TIMEOUT: Duration = Duration::from_secs(15);

///Agentless trace exporter configuration.
#[derive(Clone)]
pub struct AgentlessTraceConfig {
    /// Full URL to POST traces to (e.g.
    /// `https://public-trace-http-intake.logs.datadoghq.com/v1/input`).
    pub endpoint_url: String,
    /// Datadog API key used for the `dd-api-key` header.
    pub api_key: String,
    /// Request timeout.
    pub timeout: Duration,
}

impl Debug for AgentlessTraceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentlessTraceConfig")
            .field("endpoint_url", &self.endpoint_url)
            .field("api_key", &"<redacted>")
            .field("timeout", &self.timeout)
            .finish()
    }
}
