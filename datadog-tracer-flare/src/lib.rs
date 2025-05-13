// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{path::PathBuf, str::FromStr, time::Duration, vec};

use datadog_remote_config::{
    fetch::{ConfigInvariants, SingleChangesFetcher},
    file_change_tracker::{Change, FilePath},
    file_storage::ParsedFileStorage,
    RemoteConfigProduct, Target,
};
use ddcommon::Endpoint;
use tokio::time::sleep;

/// Represent error that can happen while using the tracer flare.
#[derive(Debug, PartialEq)]
pub enum FlareError {
    /// Send the flare was asking without being prepared.
    NoFlare(String),
    /// This was not implemented yet.
    NotImplemented,
    // TODO: Complete the enum
}

impl std::fmt::Display for FlareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlareError::NoFlare(msg) => write!(f, "No flare prepared to send: {}", msg),
            FlareError::NotImplemented => write!(f, "Not implemented"),
        }
    }
}

/// Enum that hold the different log level possible
///
/// TODO: Need to find out which other level are available
#[derive(Debug)]
pub enum LogLevel {
    Debug = 0,
    Info = 1,
    Warn = 2,
    // ...
}

/// Callback function type for preparing the tracer flare from language side
///
/// # Arguments
///
/// * `log_level` - Maximum level of log to retrieve.
type PrepFlareCallback = fn(log_level: LogLevel);

/// Callback function type for stopping the tracer flare from language side
///
/// # Return
///
/// A path to the directory where logs were put.
type StopFlareCallback = fn() -> PathBuf;

/// Function that listen to RemoteConfig on the agent
///
/// # Arguments
///
/// * `prep_flare` - Callback from language side that will be use when AGENT_CONFIG is received.
/// * `stop_flare` - Callback from language side that will be use when AGENT_TASK is received.
/// * `agent_url` - Agent url computed from the environment that will be use to listen to the remote
///   config endpoint.
///
/// # Returns
///
/// * `Ok()` - If successful.
/// * `FlareError(msg)` - If something fail.
pub async fn remote_config_listener(
    _prep_flare: PrepFlareCallback,
    _stop_flare: StopFlareCallback,
    agent_url: String,
    language: String,
    tracer_version: String,
    service: String,
    env: String,
    app_version: String,
    runtime_id: String,
) -> Result<(), FlareError> {
    let url_endpoint: String = agent_url.to_string() + "/v0.7/config";
    let remote_config_endpoint = Endpoint {
        url: hyper::Uri::from_str(&url_endpoint).unwrap(),
        api_key: None,
        timeout_ms: 10000, // 10sec
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
    let mut listener = SingleChangesFetcher::new(
        ParsedFileStorage::default(), // TODO: Maybe use SimpleFileStorage to parse by myself
        Target {
            service,
            env,
            app_version,
            tags: vec![],
        },
        runtime_id,
        config_to_fetch,
    );

    loop {
        match listener.fetch_changes().await {
            Ok(changes) => {
                println!("Got {} changes.", changes.len());
                for change in changes {
                    match change {
                        Change::Add(file) => {
                            println!("Added file: {} (version: {})", file.path(), file.version());
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
                eprintln!("Fetch failed with {e}");
            }
        }

        sleep(Duration::from_secs(3)).await;
    }

    // TODO: Add the return
    // Err(FlareError::NotImplemented)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mock callbacks
    fn default_prep_flare(log_level: LogLevel) {
        println!("Prepare of the flare with log level: {:?}", log_level);
    }
    fn default_stop_flare() -> PathBuf {
        println!("Stopping of the flare");
        PathBuf::from("/tmp/flare_output")
    }

    #[ignore]
    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_remote_config_listener() {
        // Test parameters
        let agent_url = "http://0.0.0.0:8126".to_string();
        let language = "rust".to_string();
        let tracer_version = "1.0.0".to_string();
        let service = "test-service".to_string();
        let env = "test-env".to_string();
        let app_version = "1.0.0".to_string();
        let runtime_id = "test-runtime".to_string();

        // Call the function
        let result = remote_config_listener(
            default_prep_flare,
            default_stop_flare,
            agent_url,
            language,
            tracer_version,
            service,
            env,
            app_version,
            runtime_id,
        )
        .await;

        // Verify the result
        assert!(result.is_ok());
    }
}
