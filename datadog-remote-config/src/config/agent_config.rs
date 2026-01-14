// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::Deserialize;
#[cfg(feature = "test")]
use serde::Serialize;

#[derive(Debug, Deserialize, Clone)]
#[cfg_attr(feature = "test", derive(Serialize))]
pub struct AgentConfigFile {
    pub name: String,
    pub config: AgentConfig,
}

#[derive(Debug, Deserialize, Clone)]
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
/// use datadog_remote_config::config::agent_config::parse_json;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_config_with_log_level() {
        let json_data = r#"{
            "name": "flare-log-level.debug",
            "config": {
                "log_level": "debug"
            }
        }"#;

        let result = parse_json(json_data.as_bytes());
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.name, "flare-log-level.debug");
        assert_eq!(config.config.log_level, Some("debug".to_string()));
    }

    #[test]
    fn test_valid_config_without_log_level() {
        let json_data = r#"{
            "name": "some-config",
            "config": {}
        }"#;

        let result = parse_json(json_data.as_bytes());
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.name, "some-config");
        assert_eq!(config.config.log_level, None);
    }

    #[test]
    fn test_missing_required_field_name() {
        let json_data = r#"{
            "config": {
                "log_level": "info"
            }
        }"#;

        let result = parse_json(json_data.as_bytes());
        assert!(result.is_err());
    }

    #[test]
    fn test_serialization() {
        let config = AgentConfig {
            log_level: Some("warn".to_string()),
        };

        let serialized = serde_json::to_string(&config).unwrap();
        assert!(serialized.contains("\"log_level\":\"warn\""));
    }
}
