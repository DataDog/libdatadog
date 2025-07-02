// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

pub mod error;
pub mod zip;

use std::{str::FromStr, vec};

use datadog_remote_config::{
    config::agent_task::AgentTaskFile, fetch::{ConfigInvariants, SingleChangesFetcher}, file_change_tracker::Change, file_storage::{ParsedFileStorage, RawFile, RawFileStorage}, RemoteConfigData, RemoteConfigProduct, Target
};
use ddcommon::Endpoint;
use hyper::Uri;

use crate::error::FlareError;

pub struct TracerFlare {
    pub listener: Listener,
    pub agent_url: Uri, // or maybe String if we cannot add an endpoint to an Uri
    pub language: String,
    pub previous_log_level: LogLevel, // or maybe String
    pub log_level: LogLevel, // same
    pub agent_task: AgentTaskFile,
    pub running: bool, // like the state but maybe ReturnAction would be a better state
}

/// Enum that hold the different log level possible
#[derive(Debug, PartialEq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Critical,
    Off,
}

/// Enum that hold the different returned action to do after listening
#[derive(Debug, PartialEq)]
pub enum ReturnAction {
    /// If AGENT_CONFIG received with the right properties.
    Start(LogLevel),
    /// If AGENT_TASK received with the right properties.
    Stop,
    /// If anything else received.
    None,
}

impl TryFrom<&str> for LogLevel {
    type Error = FlareError;

    fn try_from(level: &str) -> Result<Self, FlareError> {
        match level {
            "trace" => Ok(LogLevel::Trace),
            "debug" => Ok(LogLevel::Debug),
            "info" => Ok(LogLevel::Info),
            "warn" => Ok(LogLevel::Warn),
            "error" => Ok(LogLevel::Error),
            "critical" => Ok(LogLevel::Critical),
            "off" => Ok(LogLevel::Off),
            _ => Err(FlareError::ParsingError("Unknown level of log".to_string())),
        }
    }
}

pub type RemoteConfigFile = std::sync::Arc<RawFile<Result<RemoteConfigData, anyhow::Error>>>;
pub type Listener = SingleChangesFetcher<RawFileStorage<Result<RemoteConfigData, anyhow::Error>>>;
// TODO: add something to keep the state in the listener and maybe the previous log_level too
// probably the agent_url as uri too, and we'll probably need to link it to the zip and send func
// too should keep also the AgentTask since we need it when sending
// and the language

/// Check the `RemoteConfigFile` and return the action that tracer flare needs
/// to perform
///
/// # Arguments
///
/// * `file` - RemoteConfigFile received by the Listener.
///
/// # Returns
///
/// * `Ok(ReturnAction)` - If successful.
/// * `FlareError(msg)` - If something fail.
pub fn check_remote_config_file(file: RemoteConfigFile) -> Result<ReturnAction, FlareError> {
    let config = file.contents();
    match config.as_ref() {
        Ok(data) => match data {
            RemoteConfigData::TracerFlareConfig(agent_config) => {
                if agent_config.name.starts_with("flare-log-level.") {
                    if let Some(log_level) = &agent_config.config.log_level {
                        let log_level = log_level.as_str().try_into()?;
                        return Ok(ReturnAction::Start(log_level));
                    }
                }
            }
            RemoteConfigData::TracerFlareTask(agent_task) => {
                if agent_task.task_type.eq("tracer_flare") {
                    return Ok(ReturnAction::Stop);
                }
            }
            _ => return Ok(ReturnAction::None),
        },
        Err(e) => {
            return Err(FlareError::ParsingError(e.to_string()));
        }
    }
    Ok(ReturnAction::None)
}

/// Function that init and return a listener of RemoteConfig
///
/// # Arguments
///
/// * `agent_url` - Agent url computed from the environment.
/// * `language` - Language of the tracer.
/// * `tracer_version` - Version of the tracer.
/// * `service` - Service to listen to.
/// * `env` - Environment.
/// * `app_version` - Version of the application.
/// * `runtime_id` - Runtime id.
///
/// These arguments will be used to listen to the remote config endpoint.
pub fn init_remote_config_listener(
    agent_url: String,
    language: String,
    tracer_version: String,
    service: String,
    env: String,
    app_version: String,
    runtime_id: String,
) -> Result<Listener, FlareError> {
    let agent_url = match hyper::Uri::from_str(&agent_url) {
        Ok(uri) => uri,
        Err(_) => {
            return Err(FlareError::ListeningError(format!(
                "Invalid agent url: {agent_url}"
            )));
        }
    };
    let remote_config_endpoint = Endpoint {
        url: agent_url,
        ..Default::default()
    };
    let config_to_fetch = ConfigInvariants {
        language,
        tracer_version,
        endpoint: remote_config_endpoint,
        products: vec![
            RemoteConfigProduct::AgentConfig,
            RemoteConfigProduct::AgentTask,
        ],
        capabilities: vec![],
    };

    Ok(SingleChangesFetcher::new(
        ParsedFileStorage::default(),
        Target {
            service,
            env,
            app_version,
            tags: vec![],
        },
        runtime_id,
        config_to_fetch,
    ))
}

/// Function that listen to RemoteConfig on the agent
///
/// # Arguments
///
/// * `listener` - Listener use to fetch RemoteConfig from the agent with specific config.
///
/// # Returns
///
/// * `Ok(ReturnAction)` - If successful.
/// * `FlareError(msg)` - If something fail.
///
/// # Examples
///
/// Implementing and using the listener to fetch RemoteConfig from the agent
///
/// ```rust no_run
/// use datadog_tracer_flare::{init_remote_config_listener, run_remote_config_listener};
/// use std::time::Duration;
/// use tokio::time::sleep;
///
/// #[tokio::main(flavor = "current_thread")]
/// async fn main() {
///     // Setup the listener
///     let mut listener = init_remote_config_listener(
///         "http://0.0.0.0:8126".to_string(),  // agent_url
///         "rust".to_string(),                 // language
///         "1.0.0".to_string(),                // tracer_version
///         "test-service".to_string(),         // service
///         "test-env".to_string(),             // env
///         "1.0.0".to_string(),                // app_version
///         "test-runtime".to_string(),         // runtime_id
///     )
///     .unwrap();
///
///     // Listen every second
///     loop {
///         let result = run_remote_config_listener(&mut listener).await;
///         assert!(result.is_ok());
///         // Use the result ...
///         sleep(Duration::from_secs(1)).await;
///     }
/// }
/// ```
pub async fn run_remote_config_listener(
    listener: &mut Listener,
) -> Result<ReturnAction, FlareError> {
    match listener.fetch_changes().await {
        Ok(changes) => {
            println!("Got {} changes.", changes.len());
            for change in changes {
                if let Change::Add(file) = change {
                    let action = check_remote_config_file(file);
                    if action != Ok(ReturnAction::None) {
                        return action;
                    }
                }
            }
        }
        Err(e) => {
            return Err(FlareError::ListeningError(e.to_string()));
        }
    }

    Ok(ReturnAction::None)
}

#[cfg(test)]
mod tests {
    use crate::{check_remote_config_file, FlareError, LogLevel, ReturnAction};
    use datadog_remote_config::{
        config::{
            agent_config::{AgentConfig, AgentConfigFile},
            agent_task::{AgentTask, AgentTaskFile},
        },
        fetch::FileStorage,
        file_storage::ParsedFileStorage,
        RemoteConfigPath, RemoteConfigProduct, RemoteConfigSource,
    };
    use std::sync::Arc;

    #[test]
    fn test_try_from_string_to_return_action() {
        assert_eq!(LogLevel::try_from("trace").unwrap(), LogLevel::Trace);
        assert_eq!(LogLevel::try_from("debug").unwrap(), LogLevel::Debug);
        assert_eq!(LogLevel::try_from("info").unwrap(), LogLevel::Info);
        assert_eq!(LogLevel::try_from("warn").unwrap(), LogLevel::Warn);
        assert_eq!(LogLevel::try_from("error").unwrap(), LogLevel::Error);
        assert_eq!(LogLevel::try_from("critical").unwrap(), LogLevel::Critical);
        assert_eq!(LogLevel::try_from("off").unwrap(), LogLevel::Off);
        assert_eq!(
            LogLevel::try_from("anything"),
            Err(FlareError::ParsingError("Unknown level of log".to_string()))
        );
    }

    #[test]
    fn test_check_remote_config_file_with_valid_log_level() {
        let storage = ParsedFileStorage::default();
        let path = Arc::new(RemoteConfigPath {
            product: RemoteConfigProduct::AgentConfig,
            config_id: "test".to_string(),
            name: "flare-log-level.test".to_string(),
            source: RemoteConfigSource::Datadog(1),
        });

        let config = AgentConfigFile {
            name: "flare-log-level.test".to_string(),
            config: AgentConfig {
                log_level: Some("info".to_string()),
            },
        };

        let file = storage
            .store(1, path.clone(), serde_json::to_vec(&config).unwrap())
            .unwrap();
        let result = check_remote_config_file(file);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ReturnAction::Start(LogLevel::Info));
    }

    #[test]
    fn test_check_remote_config_file_with_stop_task() {
        let storage = ParsedFileStorage::default();
        let path = Arc::new(RemoteConfigPath {
            product: RemoteConfigProduct::AgentTask,
            config_id: "test".to_string(),
            name: "tracer_flare".to_string(),
            source: RemoteConfigSource::Datadog(1),
        });

        let task = AgentTaskFile {
            args: AgentTask {
                case_id: "123".to_string(),
                hostname: "test-host".to_string(),
                user_handle: "test@example.com".to_string(),
            },
            task_type: "tracer_flare".to_string(),
            uuid: "test-uuid".to_string(),
        };

        let file = storage
            .store(1, path.clone(), serde_json::to_vec(&task).unwrap())
            .unwrap();
        let result = check_remote_config_file(file);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ReturnAction::Stop);
    }

    #[test]
    fn test_check_remote_config_file_with_invalid_config() {
        let storage = ParsedFileStorage::default();
        let path = Arc::new(RemoteConfigPath {
            product: RemoteConfigProduct::AgentConfig,
            config_id: "test".to_string(),
            name: "invalid-config".to_string(),
            source: RemoteConfigSource::Datadog(1),
        });

        let config = AgentConfigFile {
            name: "invalid-config".to_string(),
            config: AgentConfig { log_level: None },
        };

        let file = storage
            .store(1, path.clone(), serde_json::to_vec(&config).unwrap())
            .unwrap();
        let result = check_remote_config_file(file);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ReturnAction::None);
    }

    #[test]
    fn test_check_remote_config_file_with_parsing_error() {
        let storage = ParsedFileStorage::default();
        let path = Arc::new(RemoteConfigPath {
            product: RemoteConfigProduct::AgentConfig,
            config_id: "test".to_string(),
            name: "invalid-json".to_string(),
            source: RemoteConfigSource::Datadog(1),
        });

        let file = storage
            .store(1, path.clone(), b"invalid json".to_vec())
            .unwrap();
        let result = check_remote_config_file(file);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), FlareError::ParsingError(_)));
    }
}
