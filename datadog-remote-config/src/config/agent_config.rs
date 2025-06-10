// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "test", derive(Serialize))]
pub struct AgentConfigFile {
    pub name: String,
    pub config: AgentConfig,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "test", derive(Serialize))]
pub struct AgentConfig {
    pub log_level: Option<String>,
}

pub fn parse_json(data: &[u8]) -> serde_json::error::Result<AgentConfigFile> {
    serde_json::from_slice(data)
}
