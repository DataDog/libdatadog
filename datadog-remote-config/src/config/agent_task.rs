// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Deserializer};
#[cfg(feature = "test")]
use serde::Serialize;

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[cfg_attr(feature = "test", derive(Serialize))]
pub struct AgentTaskFile {
    pub args: AgentTask,
    pub task_type: String,
    pub uuid: String,
}

fn non_zero_number<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let val = String::deserialize(deserializer)?;
    match val.parse() {
        Ok(val) => {
            if val == 0 {
                return Err(serde::de::Error::custom("case_id cannot be zero"));
            }
            Ok(val)
        },
        Err(_) => Err(serde::de::Error::custom("case_id must be a digit"))
    }
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[cfg_attr(feature = "test", derive(Serialize))]
pub struct AgentTask {
    #[serde(deserialize_with = "non_zero_number")]
    pub case_id: u64,
    pub hostname: String,
    pub user_handle: String,
}

/// Parses JSON data into an `AgentTaskFile` structure.
///
/// # Arguments
///
/// * `data` - A slice of bytes containing JSON data representing an agent task.
///
/// # Returns
///
/// * `Ok(AgentTaskFile)` - The parsed agent task file if successful.
/// * `Err(serde_json::error::Error)` - An error if the JSON parsing fails.
///
/// # Errors
///
/// This function will return an error if:
/// - The JSON data is malformed.
/// - The JSON structure doesn't match the expected `AgentTaskFile` format.
/// - Required fields are missing from the JSON data.
///
/// # Examples
///
/// ```
/// use datadog_remote_config::config::agent_task::parse_json;
///
/// let json_data = r#"{
///     "args": {
///         "case_id": "12345",
///         "hostname": "my-host-name",
///         "user_handle": "my-user@datadoghq.com"
///     },
///     "task_type": "tracer_flare",
///     "uuid": "550e8400-e29b-41d4-a716-446655440000"
/// }"#;
///
/// match parse_json(json_data.as_bytes()) {
///     Ok(task) => println!("Parsed task: {:?}", task),
///     Err(e) => eprintln!("Failed to parse task: {}", e),
/// }
/// ```
pub fn parse_json(data: &[u8]) -> serde_json::error::Result<AgentTaskFile> {
    serde_json::from_slice(data)
}
