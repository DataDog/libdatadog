// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

pub mod error;
pub mod zip;

use std::sync::{Arc, Mutex};

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

/// Manager for tracer flare functionality with optional remote configuration support.
///
/// The TracerFlareManager serves as the central coordinator for tracer flare operations,
/// managing the lifecycle of flare collection and transmission. It operates in two modes:
///
/// - **No listener mode**: Stores agent URL and language configuration for flare operations
/// - **Listener mode**: Listens to remote configuration updates to automatically trigger flare
///   collection and transmission
///
/// # Fields
///
/// - `agent_url`: The agent endpoint URL for flare transmission
/// - `language`: The tracer language identifier
/// - `collecting`: Current collection state (true when actively collecting)
/// - `listener`: Optional remote config listener (requires "listener" feature)
///
/// # Typical usage flow
///
/// 1. Create manager with [`new`](Self::new) for usage without listener or
///    [`new_with_listener`](Self::new_with_listener) for usage with listener
/// 2. Call [`run_remote_config_listener`] periodically to fetch and process remote config changes
/// 3. Handle returned [`ReturnAction`]: `Send(agent_task)`, `Set(log_level)`, `Unset`, or `None`
/// 4. Use the `collecting` field to track current flare collection state
pub struct TracerFlareManager {
    pub agent_url: Arc<Mutex<String>>,
    pub language: Arc<Mutex<String>>,
    pub collecting: Arc<Mutex<bool>>,
    /// As a featured option so we can use the component with no Listener
    #[cfg(feature = "listener")]
    pub listener: Arc<Mutex<Option<Listener>>>,
}

impl Default for TracerFlareManager {
    fn default() -> Self {
        TracerFlareManager {
            agent_url: Arc::new(Mutex::new(hyper::Uri::default().to_string())),
            language: Arc::new(Mutex::new("rust".to_string())),
            collecting: Arc::new(Mutex::new(false)),
            #[cfg(feature = "listener")]
            listener: Arc::new(Mutex::new(None)),
        }
    }
}

impl TracerFlareManager {
    /// Creates a new TracerFlareManager instance with basic configuration.
    ///
    /// # Arguments
    ///
    /// * `agent_url` - Agent url computed from the environment.
    /// * `language` - Language of the tracer.
    ///
    /// # Returns
    ///
    /// * `TracerFlareManager` - A new TracerFlareManager instance with basic configuration.
    ///
    /// For full RemoteConfig functionality, use `new_with_listener` instead.
    pub fn new(agent_url: &str, language: &str) -> Self {
        TracerFlareManager {
            agent_url: Arc::new(Mutex::new(agent_url.to_owned())),
            language: Arc::new(Mutex::new(language.to_owned())),
            ..Default::default()
        }
    }

    /// Creates a new TracerFlareManager instance and initializes its RemoteConfig
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
    /// These arguments will be used to listen to the remote config endpoint.
    ///
    /// # Returns
    ///
    /// * `Ok(TracerFlareManager)` - A fully initialized TracerFlareManager instance with
    ///   RemoteConfig listener.
    /// * `Err(FlareError)` - If the initialization fails.
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

        tracer_flare.listener = Arc::new(Mutex::new(Some(SingleChangesFetcher::new(
            ParsedFileStorage::default(),
            Target {
                service,
                env,
                app_version,
                tags: vec![],
            },
            runtime_id,
            config_to_fetch,
        ))));

        Ok(tracer_flare)
    }

    /// Handle the `RemoteConfigData` and return the action the tracer flare
    /// needs to perform. This function also updates the `TracerFlareManager`
    /// state based on the received configuration.
    ///
    /// # Arguments
    ///
    /// * `data` - RemoteConfigData.
    /// * `tracer_flare` - TracerFlareManager object to update with the received configuration.
    ///
    /// # Returns
    ///
    /// * `Ok(ReturnAction)` - If successful.
    /// * `FlareError(msg)` - If something fails.
    pub fn handle_remote_config_data(
        &self,
        data: &RemoteConfigData,
    ) -> Result<ReturnAction, FlareError> {
        let action = data.try_into();
        {
            let mut collecting = self.collecting.lock().unwrap();
            if let Ok(ReturnAction::Set(_)) = action {
                if *collecting {
                    return Ok(ReturnAction::None);
                }
                *collecting = true;
            } else if Ok(ReturnAction::None) != action {
                // If action is Send, Unset or an error, we need to stop collecting
                *collecting = false;
            }
        }
        action
    }

    /// Handle the `RemoteConfigFile` and return the action that tracer flare needs
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
    pub fn handle_remote_config_file(
        &self,
        file: RemoteConfigFile,
    ) -> Result<ReturnAction, FlareError> {
        match file.contents().as_ref() {
            Ok(data) => self.handle_remote_config_data(data),
            Err(e) => {
                // If encounter an error we need to stop collecting
                *self.collecting.lock().unwrap() = false;
                Err(FlareError::ParsingError(e.to_string()))
            }
        }
    }
}

/// Enum that holds the different log levels possible
/// Do not change the order of the variants because we rely on Ord
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Critical,
    Off,
}

/// Enum that holds the different return actions to perform after listening
#[derive(Debug, PartialEq, Clone)]
pub enum ReturnAction {
    /// If AGENT_TASK received with the right properties.
    ///
    /// Trigger to collect the flare and send it to the agent.
    Send(AgentTaskFile),
    /// If AGENT_CONFIG received with the right properties.
    ///
    /// Trigger to set the log level of the tracer.
    Set(LogLevel),
    /// If AGENT_CONFIG is removed.
    ///
    /// Trigger to and unset the log level.
    Unset,
    /// If anything else received.
    None,
}

impl ReturnAction {
    /// A priority is used to know which action to handle when receiving multiple RemoteConfigFile
    /// at the same time. Here is the specific order implemented :
    /// 1. Add an AGENT_TASK : `Send(agent_task)`
    /// 2. Add an AGENT_CONFIG : `Set(log_level)`
    /// 3. Remove an AGENT_CONFIG : `Unset`
    /// 4. Anything else : `None`
    fn priority(self, other: Self) -> Self {
        match &self {
            ReturnAction::Send(_) => self,
            ReturnAction::Set(self_level) => match &other {
                ReturnAction::Send(_) => other,
                ReturnAction::Set(other_level) => {
                    if self_level <= other_level {
                        return self;
                    }
                    other
                }
                _ => self,
            },
            ReturnAction::Unset => {
                if other == ReturnAction::None {
                    return self;
                }
                other
            }
            _ => other,
        }
    }
}

impl TryFrom<&str> for LogLevel {
    type Error = FlareError;

    fn try_from(level: &str) -> Result<Self, FlareError> {
        match level.to_lowercase().as_str() {
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

impl TryFrom<RemoteConfigFile> for ReturnAction {
    type Error = FlareError;

    /// Check the `RemoteConfigFile` and return the action that tracer flare needs
    /// to perform.
    ///
    /// # Arguments
    ///
    /// * `file` - RemoteConfigFile received by the Listener.
    ///
    /// # Returns
    ///
    /// * `Ok(ReturnAction)` - If successful.
    /// * `FlareError(msg)` - If something fail.
    fn try_from(file: RemoteConfigFile) -> Result<Self, Self::Error> {
        match file.contents().as_ref() {
            Ok(data) => data.try_into(),
            Err(e) => Err(FlareError::ParsingError(e.to_string())),
        }
    }
}

impl TryFrom<&RemoteConfigData> for ReturnAction {
    type Error = FlareError;

    /// Check the `&RemoteConfigData` and return the action the tracer flare
    /// needs to perform.
    ///
    /// # Arguments
    ///
    /// * `data` - &RemoteConfigData
    ///
    /// # Returns
    ///
    /// * `Ok(ReturnAction)` - If successful
    /// * `FlareError(msg)` - If something fails
    fn try_from(data: &RemoteConfigData) -> Result<Self, Self::Error> {
        match data {
            RemoteConfigData::TracerFlareConfig(agent_config) => {
                if agent_config.name.starts_with("flare-log-level.") {
                    if let Some(log_level) = &agent_config.config.log_level {
                        let log_level = log_level.as_str().try_into()?;
                        return Ok(ReturnAction::Set(log_level));
                    }
                }
            }
            RemoteConfigData::TracerFlareTask(agent_task) => {
                if agent_task.task_type.eq("tracer_flare") {
                    return Ok(ReturnAction::Send(agent_task.to_owned()));
                }
            }
            _ => return Ok(ReturnAction::None),
        }

        Ok(ReturnAction::None)
    }
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
    tracer_flare: &TracerFlareManager,
) -> Result<ReturnAction, FlareError> {
    let mut l = tracer_flare.listener.lock().unwrap();
    let listener = match &mut *l {
        Some(listener) => listener,
        None => {
            return Err(FlareError::ListeningError(
                "Listener not initialized".to_string(),
            ))
        }
    };
    let mut state = ReturnAction::None;
    match listener.fetch_changes().await {
        Ok(changes) => {
            for change in changes {
                if let Change::Add(file) = change {
                    match file.try_into() {
                        Ok(action) => state = ReturnAction::priority(action, state),
                        Err(err) => return Err(err),
                    }
                } else if let Change::Remove(file) = change {
                    match file.contents().as_ref() {
                        Ok(data) => match data {
                            RemoteConfigData::TracerFlareConfig(_) => {
                                if state == ReturnAction::None {
                                    state = ReturnAction::Unset;
                                }
                            }
                            _ => continue,
                        },
                        Err(e) => {
                            return Err(FlareError::ParsingError(e.to_string()));
                        }
                    }
                }
            }
        }
        Err(e) => {
            return Err(FlareError::ListeningError(e.to_string()));
        }
    }

    {
        let mut collecting = tracer_flare.collecting.lock().unwrap();
        if let ReturnAction::Set(_) = state {
            *collecting = true;
        } else if let ReturnAction::Send(_) = state {
            *collecting = false;
        }
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use crate::{FlareError, LogLevel, ReturnAction};
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
    fn test_log_level_ordering() {
        // Test that the ordering is maintained as expected (Trace < Debug < Info < Warn < Error <
        // Critical < Off)
        assert!(LogLevel::Trace < LogLevel::Debug);
        assert!(LogLevel::Debug < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Error);
        assert!(LogLevel::Error < LogLevel::Critical);
        assert!(LogLevel::Critical < LogLevel::Off);
    }

    #[test]
    fn test_priority_in_return_action() {
        // Test that when two Set actions are compared, the one with lower log level wins
        let send_action = ReturnAction::Send(AgentTaskFile {
            args: AgentTask {
                case_id: NonZeroU64::new(123).unwrap(),
                hostname: "test-host".to_string(),
                user_handle: "test@example.com".to_string(),
            },
            task_type: "tracer_flare".to_string(),
            uuid: "test_uuid".to_string(),
        });
        let trace_action = ReturnAction::Set(LogLevel::Trace);
        let off_action = ReturnAction::Set(LogLevel::Off);
        let unset_action = ReturnAction::Unset;
        let none_action = ReturnAction::None;

        // Lower log levels should have priority (trace < debug < info < ... < off)
        assert_eq!(
            send_action.clone().priority(trace_action.clone()),
            send_action
        );
        assert_eq!(
            trace_action.clone().priority(off_action.clone()),
            trace_action
        );
        assert_eq!(
            off_action.clone().priority(unset_action.clone()),
            off_action
        );
        assert_eq!(
            unset_action.clone().priority(none_action.clone()),
            unset_action
        );

        // Test reverse order
        assert_eq!(
            trace_action.clone().priority(send_action.clone()),
            send_action
        );
        assert_eq!(
            off_action.clone().priority(trace_action.clone()),
            trace_action
        );
        assert_eq!(
            unset_action.clone().priority(off_action.clone()),
            off_action
        );
        assert_eq!(
            none_action.clone().priority(unset_action.clone()),
            unset_action
        );
    }

    #[test]
    fn test_remote_config_with_valid_log_level() {
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
        let result = ReturnAction::try_from(file);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ReturnAction::Set(LogLevel::Info));
    }

    #[test]
    fn test_remote_config_with_send_task() {
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
        let result = ReturnAction::try_from(file);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ReturnAction::Send(task));
    }

    #[test]
    fn test_remote_config_with_invalid_config() {
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
        let result = ReturnAction::try_from(file);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ReturnAction::None);
    }

    #[test]
    fn test_handle_remote_config_file() {
        use crate::TracerFlareManager;
        let mut tracer_flare = TracerFlareManager::new("http://localhost:8126", "rust");
        let storage = ParsedFileStorage::default();

        let agent_config_file = storage
            .store(
                1,
                Arc::new(RemoteConfigPath {
                    product: RemoteConfigProduct::AgentConfig,
                    config_id: "test".to_string(),
                    name: "flare-log-level.test".to_string(),
                    source: RemoteConfigSource::Datadog(1),
                }),
                serde_json::to_vec(&AgentConfigFile {
                    name: "flare-log-level.test".to_string(),
                    config: AgentConfig {
                        log_level: Some("info".to_string()),
                    },
                })
                .unwrap(),
            )
            .unwrap();

        // First AGENT_CONFIG
        assert!(!*tracer_flare.collecting.lock().unwrap());
        let result = tracer_flare
            .handle_remote_config_file(agent_config_file.clone())
            .unwrap();
        assert_eq!(result, ReturnAction::Set(LogLevel::Info));
        assert!(*tracer_flare.collecting.lock().unwrap());

        // Second AGENT_CONFIG
        let result = tracer_flare
            .handle_remote_config_file(agent_config_file)
            .unwrap();
        assert_eq!(result, ReturnAction::None);
        assert!(*tracer_flare.collecting.lock().unwrap());

        // Non-None actions stop collecting
        let error_file = storage
            .store(
                2,
                Arc::new(RemoteConfigPath {
                    product: RemoteConfigProduct::AgentConfig,
                    config_id: "error".to_string(),
                    name: "error".to_string(),
                    source: RemoteConfigSource::Datadog(1),
                }),
                b"invalid".to_vec(),
            )
            .unwrap();

        let _ = tracer_flare.handle_remote_config_file(error_file);
        assert!(!*tracer_flare.collecting.lock().unwrap());
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
        let result = ReturnAction::try_from(file);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), FlareError::ParsingError(_)));
    }
}
