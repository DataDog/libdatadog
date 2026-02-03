// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[repr(C)]
pub struct ProcInfo {
    pid: u32,
    /// Optional crashing thread id; 0 means unset.
    tid: u32,
}

impl TryFrom<ProcInfo> for libdd_crashtracker::ProcInfo {
    type Error = anyhow::Error;
    fn try_from(value: ProcInfo) -> anyhow::Result<Self> {
        let tid = if value.tid == 0 {
            None
        } else {
            Some(value.tid)
        };
        Ok(Self {
            pid: value.pid,
            tid,
        })
    }
}
