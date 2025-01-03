// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::data::metrics;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Hash, PartialEq, Eq, Clone, Default)]
pub struct Dependency {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Hash, PartialEq, Eq, Clone, Default)]
pub struct Integration {
    pub name: String,
    pub enabled: bool,
    pub version: Option<String>,
    pub compatible: Option<bool>,
    pub auto_enabled: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Hash, PartialEq, Eq, Clone)]
pub struct Configuration {
    pub name: String,
    pub value: String,
    pub origin: ConfigurationOrigin,
}

#[derive(Serialize, Deserialize, Debug, Hash, PartialEq, Eq, Clone)]
#[repr(C)]
pub enum ConfigurationOrigin {
    EnvVar,
    Code,
    DdConfig,
    RemoteConfig,
    Default,
}

#[derive(Serialize, Debug)]
pub struct AppStarted {
    pub configuration: Vec<Configuration>,
}

#[derive(Serialize, Debug)]
pub struct AppDependenciesLoaded {
    pub dependencies: Vec<Dependency>,
}

#[derive(Serialize, Debug)]
pub struct AppIntegrationsChange {
    pub integrations: Vec<Integration>,
}

#[derive(Debug, Serialize)]
pub struct AppClientConfigurationChange {
    pub configuration: Vec<Configuration>,
}

#[derive(Serialize, Debug)]
pub struct GenerateMetrics {
    pub series: Vec<metrics::Serie>,
}

#[derive(Serialize, Debug)]
pub struct Distributions {
    pub series: Vec<metrics::Distribution>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Log {
    pub message: String,
    pub level: LogLevel,
    pub count: u32,

    #[serde(default)]
    pub stack_trace: Option<String>,
    #[serde(default)]
    pub tags: String,
    #[serde(default)]
    pub is_sensitive: bool,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "UPPERCASE")]
#[repr(C)]
pub enum LogLevel {
    Error,
    Warn,
    Debug,
}
