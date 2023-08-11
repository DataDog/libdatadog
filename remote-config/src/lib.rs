// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

mod parse;
mod targets;
pub mod fetch;
pub mod dynamic_configuration;

use serde::{Deserialize, Serialize};
pub use parse::*;

#[derive(Debug, Deserialize, Serialize, Clone, Hash, Ord, PartialOrd, Eq, PartialEq)]
pub struct Target {
    pub service: String,
    pub env: String,
    pub app_version: String,
}
