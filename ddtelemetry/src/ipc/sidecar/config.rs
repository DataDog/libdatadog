// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::{path::PathBuf, time::Duration};

use ddcommon::parse_uri;

const ENV_SIDECAR_IPC_MODE: &str = "_DD_DEBUG_SIDECAR_IPC_MODE";
const SIDECAR_IPC_MODE_SHARED: &str = "shared";
const SIDECAR_IPC_MODE_PER_PROCESS: &str = "instance_per_process";

const ENV_SIDECAR_LOG_METHOD: &str = "_DD_DEBUG_SIDECAR_LOG_METHOD";
const SIDECAR_LOG_METHOD_DISABLED: &str = "disabled";
const SIDECAR_LOG_METHOD_STDOUT: &str = "stdout";
const SIDECAR_LOG_METHOD_STDERR: &str = "stderr";

const SIDECAR_HELP: &str = "help";

const ENV_IDLE_LINGER_TIME_SECS: &str = "_DD_DEBUG_SIDECAR_IDLE_LINGER_TIME_SECS";
const DEFAULT_IDLE_LINGER_TIME: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub enum IpcMode {
    Shared,
    InstancePerProcess,
}

impl Default for IpcMode {
    fn default() -> Self {
        Self::Shared
    }
}

#[derive(Debug)]
pub enum LogMethod {
    Stdout,
    Stderr,
    File(PathBuf),
    Disabled,
}

impl Default for LogMethod {
    fn default() -> Self {
        Self::Disabled
    }
}

#[derive(Debug)]
pub struct Config {
    pub ipc_mode: IpcMode,
    pub log_method: LogMethod,
    pub idle_linger_time: Duration,
}

impl Config {
    pub fn get() -> Self {
        FromEnv::config()
    }
}

pub struct FromEnv {}

impl FromEnv {
    fn ipc_mode() -> IpcMode {
        let mode = std::env::var(ENV_SIDECAR_IPC_MODE).unwrap_or_default();

        match mode.as_str() {
            SIDECAR_IPC_MODE_SHARED => IpcMode::Shared,
            SIDECAR_IPC_MODE_PER_PROCESS => IpcMode::InstancePerProcess,
            SIDECAR_HELP => {
                println!("help: {ENV_SIDECAR_IPC_MODE}: {SIDECAR_IPC_MODE_SHARED}|{SIDECAR_IPC_MODE_PER_PROCESS}");
                IpcMode::default()
            }
            _ => IpcMode::default(),
        }
    }

    fn log_method() -> LogMethod {
        let method = std::env::var(ENV_SIDECAR_LOG_METHOD).unwrap_or_default();

        match method.as_str() {
            SIDECAR_LOG_METHOD_DISABLED => LogMethod::Disabled,
            SIDECAR_LOG_METHOD_STDOUT => LogMethod::Stdout,
            SIDECAR_LOG_METHOD_STDERR => LogMethod::Stderr,
            SIDECAR_HELP => {
                println!("help: {ENV_SIDECAR_LOG_METHOD}: {SIDECAR_LOG_METHOD_DISABLED}|{SIDECAR_LOG_METHOD_STDOUT}|{SIDECAR_LOG_METHOD_STDERR}|file:///path/to/file");
                LogMethod::default()
            }
            _ => parse_uri(method.as_str())
                .ok()
                .and_then(|u| {
                    if Some("file") == u.scheme_str() {
                        Some(LogMethod::File(PathBuf::from(u.path())))
                    } else {
                        None
                    }
                })
                .unwrap_or_default(),
        }
    }

    fn idle_linger_time() -> Duration {
        std::env::var(ENV_IDLE_LINGER_TIME_SECS)
            .unwrap_or_default()
            .parse()
            .ok()
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_IDLE_LINGER_TIME)
    }

    pub fn config() -> Config {
        Config {
            ipc_mode: Self::ipc_mode(),
            log_method: Self::log_method(),
            idle_linger_time: Self::idle_linger_time(),
        }
    }
}
