// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProcInfo {
    pub pid: u32,
}

#[cfg(test)]
impl super::test_utils::TestInstance for ProcInfo {
    fn test_instance(seed: u64) -> Self {
        Self { pid: seed as u32 }
    }
}
