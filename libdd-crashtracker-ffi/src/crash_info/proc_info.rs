// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[repr(C)]
pub struct ProcInfo {
    pid: u32,
}

impl TryFrom<ProcInfo> for datadog_crashtracker::ProcInfo {
    type Error = anyhow::Error;
    fn try_from(value: ProcInfo) -> anyhow::Result<Self> {
        Ok(Self { pid: value.pid })
    }
}
