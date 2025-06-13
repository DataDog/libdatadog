// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::Deserialize;
#[cfg(feature = "test")]
use serde::Serialize;

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "test", derive(Serialize))]
pub struct AgentTaskFile {
    pub args: AgentTask,
    pub task_type: String,
    pub uuid: String,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "test", derive(Serialize))]
pub struct AgentTask {
    pub case_id: String,
    pub hostname: Option<String>,
    pub user_handle: String,
}

pub fn parse_json(data: &[u8]) -> serde_json::error::Result<AgentTaskFile> {
    serde_json::from_slice(data)
}
