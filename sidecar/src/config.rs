// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use http::uri::{PathAndQuery, Scheme};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf, time::Duration};

use ddcommon_net1::Endpoint;
use spawn_worker::LibDependency;

const ENV_SIDECAR_IPC_MODE: &str = "_DD_DEBUG_SIDECAR_IPC_MODE";
const SIDECAR_IPC_MODE_SHARED: &str = "shared";
const SIDECAR_IPC_MODE_PER_PROCESS: &str = "instance_per_process";

const ENV_SIDECAR_LOG_LEVEL: &str = "_DD_DEBUG_SIDECAR_LOG_LEVEL";

const ENV_SIDECAR_LOG_METHOD: &str = "_DD_DEBUG_SIDECAR_LOG_METHOD";
const SIDECAR_LOG_METHOD_DISABLED: &str = "disabled";
const SIDECAR_LOG_METHOD_STDOUT: &str = "stdout";
const SIDECAR_LOG_METHOD_STDERR: &str = "stderr"; // https://github.com/tokio-rs/tokio/issues/5866

const SIDECAR_HELP: &str = "help";

const ENV_IDLE_LINGER_TIME_SECS: &str = "_DD_DEBUG_SIDECAR_IDLE_LINGER_TIME_SECS";
const DEFAULT_IDLE_LINGER_TIME: Duration = Duration::from_secs(60);

const ENV_SIDECAR_SELF_TELEMETRY: &str = "_DD_SIDECAR_SELF_TELEMETRY";

const ENV_SIDECAR_APPSEC_SHARED_LIB_PATH: &str = "_DD_SIDECAR_APPSEC_SHARED_LIB_PATH";
const ENV_SIDECAR_APPSEC_SOCKET_FILE_PATH: &str = "_DD_SIDECAR_APPSEC_SOCKET_FILE_PATH";
const ENV_SIDECAR_APPSEC_LOCK_FILE_PATH: &str = "_DD_SIDECAR_APPSEC_LOCK_FILE_PATH";
const ENV_SIDECAR_APPSEC_LOG_FILE_PATH: &str = "_DD_SIDECAR_APPSEC_LOG_FILE_PATH";
const ENV_SIDECAR_APPSEC_LOG_LEVEL: &str = "_DD_SIDECAR_APPSEC_LOG_LEVEL";

#[derive(Debug, Copy, Clone)]
pub enum IpcMode {
    Shared,
    InstancePerProcess,
}

impl Default for IpcMode {
    fn default() -> Self {
        Self::Shared
    }
}

impl std::fmt::Display for IpcMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IpcMode::Shared => write!(f, "{SIDECAR_IPC_MODE_SHARED}"),
            IpcMode::InstancePerProcess => write!(f, "{SIDECAR_IPC_MODE_PER_PROCESS}"),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
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

impl std::fmt::Display for LogMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogMethod::Disabled => write!(f, "{SIDECAR_LOG_METHOD_DISABLED}"),
            LogMethod::Stdout => write!(f, "{SIDECAR_LOG_METHOD_STDOUT}"),
            LogMethod::Stderr => write!(f, "{SIDECAR_LOG_METHOD_STDERR}"),
            LogMethod::File(path) => write!(f, "file://{}", path.to_string_lossy()),
        }
    }
}

#[derive(Debug)]
pub struct Config {
    pub ipc_mode: IpcMode,
    pub log_method: LogMethod,
    pub log_level: String,
    pub idle_linger_time: Duration,
    pub self_telemetry: bool,
    pub library_dependencies: Vec<LibDependency>,
    pub child_env: HashMap<std::ffi::OsString, std::ffi::OsString>,
    pub appsec_config: Option<AppSecConfig>,
}

#[derive(Debug, Clone)]
pub struct AppSecConfig {
    pub shared_lib_path: std::ffi::OsString,
    pub socket_file_path: std::ffi::OsString,
    pub lock_file_path: std::ffi::OsString,
    pub log_file_path: std::ffi::OsString,
    pub log_level: String,
}

impl Config {
    pub fn get() -> Self {
        FromEnv::config()
    }

    pub fn to_env(&self) -> HashMap<&'static str, std::ffi::OsString> {
        let mut res = HashMap::from([
            (ENV_SIDECAR_IPC_MODE, self.ipc_mode.to_string().into()),
            (ENV_SIDECAR_LOG_METHOD, self.log_method.to_string().into()),
            (
                ENV_IDLE_LINGER_TIME_SECS,
                self.idle_linger_time.as_secs().to_string().into(),
            ),
            (
                ENV_SIDECAR_SELF_TELEMETRY,
                self.self_telemetry.to_string().into(),
            ),
        ]);
        if self.appsec_config.is_some() {
            res.extend(self.appsec_config.as_ref().unwrap().to_env());
        }
        res
    }
}

impl AppSecConfig {
    pub fn to_env(&self) -> HashMap<&'static str, std::ffi::OsString> {
        HashMap::from([
            (
                ENV_SIDECAR_APPSEC_SHARED_LIB_PATH,
                self.shared_lib_path.to_owned(),
            ),
            (
                ENV_SIDECAR_APPSEC_SOCKET_FILE_PATH,
                self.socket_file_path.to_owned(),
            ),
            (
                ENV_SIDECAR_APPSEC_LOCK_FILE_PATH,
                self.lock_file_path.to_owned(),
            ),
            (
                ENV_SIDECAR_APPSEC_LOG_FILE_PATH,
                self.log_file_path.to_owned(),
            ),
            (
                ENV_SIDECAR_APPSEC_LOG_LEVEL,
                self.log_level.to_owned().into(),
            ),
        ])
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

    pub fn log_method() -> LogMethod {
        let method = std::env::var(ENV_SIDECAR_LOG_METHOD).unwrap_or_default();

        match method.as_str() {
            SIDECAR_LOG_METHOD_DISABLED => LogMethod::Disabled,
            SIDECAR_LOG_METHOD_STDOUT => LogMethod::Stdout,
            SIDECAR_LOG_METHOD_STDERR => LogMethod::Stderr,
            SIDECAR_HELP => {
                println!("help: {ENV_SIDECAR_LOG_METHOD}: {SIDECAR_LOG_METHOD_DISABLED}|{SIDECAR_LOG_METHOD_STDOUT}|{SIDECAR_LOG_METHOD_STDERR}|file:///path/to/file");
                LogMethod::default()
            }
            method if method.starts_with("file://") => {
                // not a real uri, just a plain (unencoded) path prefixed
                // with file://
                LogMethod::File(PathBuf::from(&method[7..]))
            }
            _ => LogMethod::default(),
        }
    }

    pub fn log_level() -> String {
        std::env::var(ENV_SIDECAR_LOG_LEVEL).unwrap_or_default()
    }

    fn idle_linger_time() -> Duration {
        std::env::var(ENV_IDLE_LINGER_TIME_SECS)
            .unwrap_or_default()
            .parse()
            .ok()
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_IDLE_LINGER_TIME)
    }

    fn self_telemetry() -> bool {
        matches!(
            std::env::var(ENV_SIDECAR_SELF_TELEMETRY).as_deref(),
            Ok("true" | "1")
        )
    }

    pub fn config() -> Config {
        Config {
            ipc_mode: Self::ipc_mode(),
            log_method: Self::log_method(),
            log_level: Self::log_level(),
            idle_linger_time: Self::idle_linger_time(),
            self_telemetry: Self::self_telemetry(),
            library_dependencies: vec![],
            child_env: std::env::vars_os().collect(),
            appsec_config: Self::appsec_config(),
        }
    }

    pub fn appsec_config() -> Option<AppSecConfig> {
        let shared_lib_path = std::env::var_os(ENV_SIDECAR_APPSEC_SHARED_LIB_PATH)?;
        let socket_file_path = std::env::var_os(ENV_SIDECAR_APPSEC_SOCKET_FILE_PATH)?;
        let lock_file_path = std::env::var_os(ENV_SIDECAR_APPSEC_LOCK_FILE_PATH)?;
        let log_file_path = std::env::var_os(ENV_SIDECAR_APPSEC_LOG_FILE_PATH)?;
        let log_level = std::env::var(ENV_SIDECAR_APPSEC_LOG_LEVEL).ok()?;

        Some(AppSecConfig {
            shared_lib_path,
            socket_file_path,
            lock_file_path,
            log_file_path,
            log_level,
        })
    }
}

pub fn get_product_endpoint(subdomain: &str, endpoint: &Endpoint) -> Endpoint {
    if let Some(ref api_key) = endpoint.api_key {
        let mut parts = endpoint.url.clone().into_parts();
        if parts.scheme.is_none() {
            parts.scheme = Some(Scheme::HTTPS);
            parts.authority = Some(
                format!("{}.{}", subdomain, parts.authority.unwrap())
                    .parse()
                    .unwrap(),
            );
        }
        parts.path_and_query = Some(PathAndQuery::from_static("/"));
        Endpoint {
            url: hyper::Uri::from_parts(parts).unwrap(),
            api_key: Some(api_key.clone()),
            test_token: endpoint.test_token.clone(),
            ..*endpoint
        }
    } else {
        endpoint.clone()
    }
}
