// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Metadata {
    pub library_name: String,
    pub library_version: String,
    pub family: String,
    // Should include "service", "environment", etc
    pub tags: Vec<String>,
}

impl From<crate::crash_info::internal::CrashtrackerMetadata> for Metadata {
    fn from(value: crate::crash_info::internal::CrashtrackerMetadata) -> Self {
        Self {
            library_name: value.library_name,
            library_version: value.library_version,
            family: value.family,
            tags: value
                .tags
                .iter()
                .map(ddcommon::tag::Tag::to_string)
                .collect(),
        }
    }
}
