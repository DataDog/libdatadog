// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    pub library_name: String,
    pub library_version: String,
    pub family: String,
    // Should include "service", "environment", etc
    pub tags: Vec<String>,
}
