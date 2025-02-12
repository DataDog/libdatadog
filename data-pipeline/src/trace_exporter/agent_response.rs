// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// `AgentResponse` structure holds agent response information upon successful request.
#[derive(Debug, PartialEq)]
pub struct AgentResponse {
    /// Sampling rate for the current service.
    pub body: String,
}

impl From<String> for AgentResponse {
    fn from(value: String) -> Self {
        AgentResponse { body: value }
    }
}
