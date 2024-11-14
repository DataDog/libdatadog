// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OsInfo {
    pub architecture: String,
    pub bitness: String,
    pub os_type: String,
    pub version: String,
}

impl From<os_info::Info> for OsInfo {
    fn from(value: os_info::Info) -> Self {
        let architecture = value.architecture().unwrap_or("unknown").to_string();
        let bitness = value.bitness().to_string();
        let os_type = value.os_type().to_string();
        let version = value.version().to_string();
        Self {
            architecture,
            bitness,
            os_type,
            version,
        }
    }
}
