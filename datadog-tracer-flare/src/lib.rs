// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

pub mod error;
pub mod zip;

use datadog_remote_config::{
    config::agent_task::AgentTaskFile, file_storage::RawFile, RemoteConfigData,
};

#[cfg(feature = "listener")]
use {
    datadog_remote_config::{
        fetch::{ConfigInvariants, SingleChangesFetcher},
        file_change_tracker::Change,
        file_storage::{ParsedFileStorage, RawFileStorage},
        RemoteConfigProduct, Target,
    },
    ddcommon::Endpoint,
    std::str::FromStr,
};

use crate::error::FlareError;

pub struct TracerFlareManager {
    pub agent_url: String,
    pub language: String,
    pub state: State,
    #[cfg(feature = "listener")]
    pub listener: Option<Listener>, /* As a featured option so we can use the component with no
                                     * Listener */
}

#[derive(Debug, PartialEq)]
pub enum State {
    Idle,
    Collecting {
        log_level: String,
    },
    Sending {
        agent_task: AgentTaskFile,
        log_level: String,
    },
}

impl Default for TracerFlareManager {
    fn default() -> Self {
        TracerFlareManager {
            agent_url: hyper::Uri::default().to_string(),
            language: "rust".to_string(),
            state: State::Idle,
            #[cfg(feature = "listener")]
            listener: None,
        }
    }
}

impl TracerFlareManager {
    pub fn new(agent_url: &str, language: &str) -> Self {
        TracerFlareManager {
            agent_url: agent_url.to_owned(),
            language: language.to_owned(),
            ..Default::default()
        }
    }

    /// Function that creates a new TracerFlareManager instance and initializes its RemoteConfig
    /// listener with the provided configuration parameters.
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
    /// # Returns
    ///
    /// * `Ok(TracerFlareManager)` - A fully initialized TracerFlareManager instance with
    ///   RemoteConfig listener.
    /// * `Err(FlareError)` - If the initialization fails.
    ///
    /// These arguments will be used to listen to the remote config endpoint.
    #[cfg(feature = "listener")]
    pub fn new_with_listener(
        agent_url: String,
        language: String,
        tracer_version: String,
        service: String,
        env: String,
        app_version: String,
        runtime_id: String,
    ) -> Result<Self, FlareError> {
        let mut tracer_flare = Self::new(&agent_url, &language);

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

        tracer_flare.listener = Some(SingleChangesFetcher::new(
            ParsedFileStorage::default(),
            Target {
                service,
                env,
                app_version,
                tags: vec![],
            },
            runtime_id,
            config_to_fetch,
        ));

        Ok(tracer_flare)
    }
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
#[cfg(feature = "listener")]
pub type Listener = SingleChangesFetcher<RawFileStorage<Result<RemoteConfigData, anyhow::Error>>>;

/// Check the `RemoteConfigFile` and return the action that tracer flare needs
/// to perform. This function also updates the `TracerFlareManager` state based on the
/// received configuration.
///
/// # Arguments
///
/// * `file` - RemoteConfigFile received by the Listener.
/// * `tracer_flare` - TracerFlareManager object to update with the received configuration.
///
/// # Returns
///
/// * `Ok(ReturnAction)` - If successful.
/// * `FlareError(msg)` - If something fail.
pub fn check_remote_config_file(
    file: RemoteConfigFile,
    tracer_flare: &mut TracerFlareManager,
) -> Result<ReturnAction, FlareError> {
    let config = file.contents();
    match config.as_ref() {
        Ok(data) => match data {
            RemoteConfigData::TracerFlareConfig(agent_config) => {
                if agent_config.name.starts_with("flare-log-level.") {
                    if let Some(log_level) = &agent_config.config.log_level {
                        if let State::Collecting { log_level: _ } = tracer_flare.state {
                            // Should we return an error instead if we are trying to launch another
                            // flare while one is already running ?
                            return Ok(ReturnAction::None);
                        }
                        tracer_flare.state = State::Collecting {
                            log_level: log_level.to_string(),
                        };
                        let log_level = log_level.as_str().try_into()?;
                        return Ok(ReturnAction::Start(log_level));
                    }
                }
            }
            RemoteConfigData::TracerFlareTask(agent_task) => {
                if agent_task.task_type.eq("tracer_flare") {
                    if let State::Collecting { log_level } = &tracer_flare.state {
                        tracer_flare.state = State::Sending {
                            agent_task: agent_task.clone(),
                            log_level: log_level.to_string(),
                        };
                        return Ok(ReturnAction::Stop);
                    } else {
                        // Should we return None instead ?
                        return Err(FlareError::NoFlare(
                            "Cannot stop an inexisting flare".to_string(),
                        ));
                    }
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

/// Function that listens to RemoteConfig on the agent using the TracerFlareManager instance
///
/// This function uses the listener contained within the TracerFlareManager to fetch
/// RemoteConfig changes from the agent and processes them to determine the
/// appropriate action to take.
///
/// # Arguments
///
/// * `tracer_flare` - TracerFlareManager that holds the Listener used to fetch RemoteConfig from
///   the agent with specific config. The TracerFlareManager state will be updated based on received
///   configurations.
///
/// # Returns
///
/// * `Ok(ReturnAction)` - If successful.
/// * `FlareError(msg)` - If something fail.
///
/// # Examples
///
/// Implementing and using the tracer flare to fetch RemoteConfig from the agent
///
/// ```rust no_run
/// use datadog_tracer_flare::{TracerFlareManager, run_remote_config_listener};
/// use std::time::Duration;
/// use tokio::time::sleep;
///
/// #[tokio::main(flavor = "current_thread")]
/// async fn main() {
///     // Setup the TracerFlareManager
///     let mut tracer_flare = TracerFlareManager::new_with_listener(
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
///         let result = run_remote_config_listener(&mut tracer_flare).await;
///         assert!(result.is_ok());
///         // Use the result ...
///         sleep(Duration::from_secs(1)).await;
///     }
/// }
/// ```
#[cfg(feature = "listener")]
pub async fn run_remote_config_listener(
    tracer_flare: &mut TracerFlareManager,
) -> Result<ReturnAction, FlareError> {
    let listener = match &mut tracer_flare.listener {
        Some(listener) => listener,
        None => {
            return Err(FlareError::ListeningError(
                "Listener not initialized".to_string(),
            ))
        }
    };
    match listener.fetch_changes().await {
        Ok(changes) => {
            println!("Got {} changes.", changes.len());
            for change in changes {
                if let Change::Add(file) = change {
                    let action = check_remote_config_file(file, tracer_flare);
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
    use crate::{
        check_remote_config_file, FlareError, LogLevel, ReturnAction, State, TracerFlareManager,
    };
    use datadog_remote_config::{
        config::{
            agent_config::{AgentConfig, AgentConfigFile},
            agent_task::{AgentTask, AgentTaskFile},
        },
        fetch::FileStorage,
        file_storage::ParsedFileStorage,
        RemoteConfigPath, RemoteConfigProduct, RemoteConfigSource,
    };
    use std::{num::NonZeroU64, sync::Arc};

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
        let mut tracer_flare = TracerFlareManager::default();
        let result = check_remote_config_file(file, &mut tracer_flare);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ReturnAction::Start(LogLevel::Info));
        assert_eq!(
            tracer_flare.state,
            State::Collecting {
                log_level: "info".to_string()
            }
        );
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
                case_id: NonZeroU64::new(123).unwrap(),
                hostname: "test-host".to_string(),
                user_handle: "test@example.com".to_string(),
            },
            task_type: "tracer_flare".to_string(),
            uuid: "test-uuid".to_string(),
        };

        let file = storage
            .store(1, path.clone(), serde_json::to_vec(&task).unwrap())
            .unwrap();
        let mut tracer_flare = TracerFlareManager {
            // Emulate the start action
            state: State::Collecting {
                log_level: "debug".to_string(),
            },
            ..Default::default()
        };
        let result = check_remote_config_file(file, &mut tracer_flare);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ReturnAction::Stop);
        assert_eq!(
            tracer_flare.state,
            State::Sending {
                agent_task: task,
                log_level: "debug".to_string()
            }
        );
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
        let mut tracer_flare = TracerFlareManager::default();
        let result = check_remote_config_file(file, &mut tracer_flare);
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
        let mut tracer_flare = TracerFlareManager::default();
        let result = check_remote_config_file(file, &mut tracer_flare);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), FlareError::ParsingError(_)));
    }
}
