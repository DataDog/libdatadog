// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use serde::{Deserialize, Serialize};
use ddcommon::tag::Tag;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrashtrackerMetadata {
    pub profiling_library_name: String,
    pub profiling_library_version: String,
    pub family: String,
    // Should include "service", "environment", etc
    pub tags: Vec<Tag>,
}

impl CrashtrackerMetadata {
    pub fn new(
        profiling_library_name: String,
        profiling_library_version: String,
        family: String,
        tags: Vec<Tag>,
    ) -> Self {
        Self {
            profiling_library_name,
            profiling_library_version,
            family,
            tags,
        }
    }
}