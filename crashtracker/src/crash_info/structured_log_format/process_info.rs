// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::super::internal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
}

impl From<crate::crash_info::internal::ProcessInfo> for ProcessInfo {
    fn from(value: internal::ProcessInfo) -> Self {
        Self { pid: value.pid }
    }
}
