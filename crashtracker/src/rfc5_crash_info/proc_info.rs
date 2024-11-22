// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProcInfo {
    pid: u32,
}

impl From<crate::crash_info::ProcessInfo> for ProcInfo {
    fn from(value: crate::crash_info::ProcessInfo) -> Self {
        Self { pid: value.pid }
    }
}

#[cfg(test)]
impl super::test_utils::TestInstance for ProcInfo {
    fn test_instance(seed: u64) -> Self {
        Self { pid: seed as u32 }
    }
}
