// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::data::metrics;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub enum DependencyType {
    SharedSystemLibrary,
    PlatformStandard, // Default when not specified.
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Dependency {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(rename = "type")]
    pub type_: DependencyType, // TODO convert to enum?
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Integration {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compatible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_enabled: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AppStarted {
    pub integrations: Vec<Integration>,
    pub dependencies: Vec<Dependency>,
    pub config: Vec<(String, String)>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AppDependenciesLoaded {
    pub dependencies: Vec<Dependency>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AppIntegrationsChange {
    pub integrations: Vec<Integration>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct GenerateMetrics {
    pub namespace: String,
    pub lib_language: String,
    pub lib_version: String,
    pub series: Vec<metrics::Metric>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Log {
    pub message: String,
    pub level: LogLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_trace: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "UPPERCASE")]
#[repr(C)]
pub enum LogLevel {
    Error,
    Warn,
    Debug,
}
