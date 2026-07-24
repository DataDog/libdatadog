// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::hash::Hasher;

use crate::data::metrics;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Hash, PartialEq, Eq, Clone, Default)]
pub struct Dependency {
    pub name: String,
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Vec<DependencyMetadata>>,
}

/// SCA metadata may be attached to a dependency after it was first reported; keying the store
/// by this instead of the full struct means that update refreshes the existing entry instead of
/// being stored (and re-sent) as a second entry for the same package/version.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DependencyKey {
    name: String,
    version: Option<String>,
    hash: Option<String>,
}

impl crate::worker::store::Keyed<DependencyKey> for Dependency {
    fn key(&self) -> DependencyKey {
        DependencyKey {
            name: self.name.clone(),
            version: self.version.clone(),
            hash: self.hash.clone(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Hash, PartialEq, Eq, Clone, Default)]
pub struct DependencyMetadata {
    pub r#type: String,
    pub value: String,
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
    pub config_id: Option<String>,
    pub seq_id: Option<u64>,
}

#[derive(
    Serialize,
    Deserialize,
    Debug,
    Hash,
    PartialEq,
    Eq,
    Clone,
    Copy,
    strum_macros::Display,
    strum_macros::EnumIter,
)]
#[repr(C)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ConfigurationOrigin {
    EnvVar,
    Code,
    DdConfig,
    RemoteConfig,
    Default,
    LocalStableConfig,
    FleetStableConfig,
    Calculated,
    OtelEnvVar,
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct Error {
    pub code: Option<i64>,
    pub message: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct AppStarted {
    pub configuration: Vec<Configuration>,
    pub dependencies: Vec<Dependency>,
    pub integrations: Vec<Integration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_signature: Option<InstallSignature>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub products: HashMap<String, ProductState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}

#[derive(Serialize, Debug, Clone)]
pub struct InstallSignature {
    pub install_id: Option<String>,
    pub install_type: Option<String>,
    pub install_time: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct AppDependenciesLoaded {
    pub dependencies: Vec<Dependency>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct ProductState {
    pub enabled: bool,
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}

#[derive(Serialize, Debug)]
pub struct AppProductChange {
    pub products: HashMap<String, ProductState>,
}

#[derive(Serialize, Debug)]
pub struct AppIntegrationsChange {
    pub integrations: Vec<Integration>,
}

#[derive(Debug, Serialize)]
pub struct AppClientConfigurationChange {
    pub configuration: Vec<Configuration>,
}

#[derive(Debug, Serialize)]
pub struct AppEndpoints {
    pub is_first: bool,
    pub endpoints: Vec<serde_json::Value>,
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
    #[serde(default)]
    pub is_crash: bool,
}

#[derive(
    Serialize,
    Deserialize,
    Debug,
    PartialEq,
    Eq,
    Hash,
    Clone,
    Copy,
    strum_macros::Display,
    strum_macros::EnumIter,
)]
#[serde(rename_all = "UPPERCASE")]
#[strum(serialize_all = "UPPERCASE")]
#[repr(C)]
pub enum LogLevel {
    Error,
    Warn,
    Debug,
}

#[derive(Serialize, Debug)]
pub struct Logs {
    pub logs: Vec<Log>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "UPPERCASE")]
#[repr(C)]
pub enum Method {
    Get = 0,
    Post = 1,
    Put = 2,
    Delete = 3,
    Patch = 4,
    Head = 5,
    Options = 6,
    Trace = 7,
    Connect = 8,
    // The OpenAPI spec uses "*" for this variant. Whether "Other" accurately
    // describes the "*" concept (any method) or whether it should be a
    // catch-all for unknown methods is unclear.
    #[serde(rename = "*")]
    Other = 9,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Endpoint {
    #[serde(default)]
    pub method: Option<Method>,
    #[serde(default)]
    pub path: Option<String>,
    pub operation_name: String,
    pub resource_name: String,
}

impl PartialEq for Endpoint {
    fn eq(&self, other: &Self) -> bool {
        self.resource_name == other.resource_name
    }
}

impl Eq for Endpoint {}

impl std::hash::Hash for Endpoint {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.resource_name.hash(state);
    }
}

impl Endpoint {
    pub fn to_json_value(&self) -> serde_json::Result<serde_json::Value> {
        serde_json::to_value(self)
    }
}
