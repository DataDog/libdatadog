// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum Version {
    Unknown,
    Semantic(u64, u64, u64),
    Rolling(Option<String>),
    Custom(String),
}

impl Default for Version {
    fn default() -> Self {
        Self::Unknown
    }
}

impl From<&os_info::Version> for Version {
    fn from(value: &os_info::Version) -> Self {
        use Version::*;
        match value {
            os_info::Version::Unknown => Unknown,
            os_info::Version::Semantic(a, b, c) => Semantic(*a, *b, *c),
            os_info::Version::Rolling(a) => Rolling(a.clone()),
            os_info::Version::Custom(a) => Custom(a.to_string()),
        }
    }
}

impl Version {
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OsInfo {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    architecture: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    bitness: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    os_type: String,
    #[serde(default, skip_serializing_if = "Version::is_unknown")]
    version: Version,
}

impl From<os_info::Info> for OsInfo {
    fn from(value: os_info::Info) -> Self {
        Self {
            architecture: value.architecture().unwrap_or_default().to_string(),
            bitness: value.bitness().to_string(),
            os_type: value.os_type().to_string(),
            version: value.version().into(),
        }
    }
}
