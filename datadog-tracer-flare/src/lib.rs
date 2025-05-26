// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

use std::{str::FromStr, vec};

use datadog_remote_config::{
    fetch::{ConfigInvariants, SingleChangesFetcher},
    file_change_tracker::{Change, FilePath},
    file_storage::{ParsedFileStorage, RawFileStorage},
    RemoteConfigData, RemoteConfigProduct, Target,
};
use ddcommon::Endpoint;

/// Represent error that can happen while using the tracer flare.
#[derive(Debug, PartialEq)]
pub enum FlareError {
    /// Send the flare was asking without being prepared.
    NoFlare(String),
    /// Listening to the RemoteConfig failed.
    ListeningError(String),
}

impl std::fmt::Display for FlareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlareError::NoFlare(msg) => write!(f, "No flare prepared to send: {}", msg),
            FlareError::ListeningError(msg) => write!(f, "Listening failed with: {}", msg),
        }
    }
}

/// Enum that hold the different log level possible
#[derive(Debug)]
pub enum LogLevel {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
    Critical = 5,
    Off = 6,
}

/// Enum that hold the different returned action to do after listening
#[derive(Debug)]
pub enum ReturnAction {
    None,
    StartTrace,
    StartDebug,
    StartInfo,
    StartWarn,
    StartError,
    StartCritical,
    StartOff,
    Stop,
}

impl From<LogLevel> for ReturnAction {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Trace => ReturnAction::StartTrace,
            LogLevel::Debug => ReturnAction::StartDebug,
            LogLevel::Info => ReturnAction::StartInfo,
            LogLevel::Warn => ReturnAction::StartWarn,
            LogLevel::Error => ReturnAction::StartError,
            LogLevel::Critical => ReturnAction::StartCritical,
            LogLevel::Off => ReturnAction::StartOff,
        }
    }
}

pub type Listener = SingleChangesFetcher<RawFileStorage<Result<RemoteConfigData, anyhow::Error>>>;

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
            return Err(FlareError::ListeningError("Invalid agent url".to_string()));
        }
    };
    let remote_config_endpoint = Endpoint {
        url: agent_url,
        api_key: None,
        timeout_ms: 10_000, // 10sec
        test_token: None,
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
                match change {
                    Change::Add(file) => {
                        println!("Added file: {} (version: {})", file.path(), file.version());
                        println!("Content: {:?}", file.contents().as_ref());
                    }
                    Change::Update(file, _) => {
                        println!(
                            "Got update for file: {} (version: {})",
                            file.path(),
                            file.version()
                        );
                    }
                    Change::Remove(file) => {
                        println!("Removing file {}", file.path());
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
