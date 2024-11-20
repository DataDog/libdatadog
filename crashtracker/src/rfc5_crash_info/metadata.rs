// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::unknown_value::UnknownValue;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Metadata {
    pub library_name: String,
    pub library_version: String,
    pub family: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    /// A list of "key:value" tuples.
    pub tags: Vec<String>,
}

impl UnknownValue for Metadata {
    fn unknown_value() -> Self {
        Self {
            library_name: "unknown".to_string(),
            library_version: "unknown".to_string(),
            family: "unknown".to_string(),
            tags: vec![],
        }
    }
}

impl From<crate::crash_info::CrashtrackerMetadata> for Metadata {
    fn from(value: crate::crash_info::CrashtrackerMetadata) -> Self {
        let tags = value
            .tags
            .into_iter()
            .map(|t: ddcommon::tag::Tag| t.to_string())
            .collect();
        Self {
            library_name: value.library_name,
            library_version: value.library_version,
            family: value.family,
            tags,
        }
    }
}
