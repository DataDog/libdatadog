// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProcInfo {
    pub pid: u32,
    /// The crashing thread id. 0 means unknown/unset (consistent with the C
    /// FFI convention). The emitter always provides a real tid; only
    /// programmatic callers that do not know the tid should leave this as 0.
    /// Requires schema v1.9+; earlier versions used `Option<u32>` and may
    /// serialize this field as null, which is not accepted here.
    pub tid: u32,
}

#[cfg(test)]
impl super::test_utils::TestInstance for ProcInfo {
    fn test_instance(seed: u64) -> Self {
        Self {
            pid: seed as u32,
            tid: seed as u32 + 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProcInfo;

    #[test]
    fn test_tid_deserializes_integer() {
        let p: ProcInfo = serde_json::from_str(r#"{"pid": 1, "tid": 42}"#).unwrap();
        assert_eq!(p.tid, 42);
    }

    #[test]
    fn test_tid_zero_is_valid() {
        let p: ProcInfo = serde_json::from_str(r#"{"pid": 1, "tid": 0}"#).unwrap();
        assert_eq!(p.tid, 0);
    }
}
