// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use ddcommon::tag::Tag;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrashtrackerMetadata {
    pub library_name: String,
    pub library_version: String,
    pub family: String,
    // Should include "service", "environment", etc
    pub tags: Vec<Tag>,
}

impl CrashtrackerMetadata {
    pub fn new(
        library_name: String,
        library_version: String,
        family: String,
        tags: Vec<Tag>,
    ) -> Self {
        Self {
            library_name,
            library_version,
            family,
            tags,
        }
    }
}
