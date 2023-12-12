// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::data::metrics;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Hash, PartialEq, Eq, Clone, Default)]
pub struct Dependency {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Hash, PartialEq, Eq, Clone, Default)]
pub struct Integration {
    pub name: String,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compatible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
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

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Log {
    pub message: String,
    pub level: LogLevel,
    pub count: u32,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub stack_trace: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub tags: String,
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
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
