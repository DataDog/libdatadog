// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::Deserialize;
#[cfg(feature = "test")]
use serde::Serialize;

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

/// Parses JSON data into an `AgentConfigFile` structure.
///
/// # Arguments
///
/// * `data` - A slice of bytes containing JSON data representing an agent configuration.
///
/// # Returns
///
/// * `Ok(AgentConfigFile)` - The parsed agent configuration file if successful.
/// * `Err(serde_json::error::Error)` - An error if the JSON parsing fails.
///
/// # Errors
///
/// This function will return an error if:
/// - The JSON data is malformed.
/// - The JSON structure doesn't match the expected `AgentConfigFile` format.
/// - Required fields are missing from the JSON data.
///
/// # Examples
///
/// ```
/// use libdd_remote_config::config::agent_config::parse_json;
///
/// let json_data = r#"{
///     "name": "flare-log-level.debug",
///     "config": {
///         "log_level": "debug"
///     }
/// }"#;
///
/// match parse_json(json_data.as_bytes()) {
///     Ok(config) => println!("Parsed config: {:?}", config),
///     Err(e) => eprintln!("Failed to parse config: {}", e),
/// }
/// ```
pub fn parse_json(data: &[u8]) -> serde_json::error::Result<AgentConfigFile> {
    serde_json::from_slice(data)
}
