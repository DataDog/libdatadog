// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_crashtracker::{SiCodes, SignalNames};
use ddcommon_ffi::ToHexStr;

#[repr(C)]
pub struct SigInfo {
    pub addr: usize,
    pub code: libc::c_int,
    pub code_human_readable: SiCodes,
    pub signo: libc::c_int,
    pub signo_human_readable: SignalNames,
}

impl TryFrom<SigInfo> for datadog_crashtracker::SigInfo {
    type Error = anyhow::Error;
    fn try_from(value: SigInfo) -> anyhow::Result<Self> {
        Ok(Self {
            si_addr: Some(value.addr.to_hex_str()),
            si_code: value.code,
            si_code_human_readable: value.code_human_readable,
            si_signo: value.signo,
            si_signo_human_readable: value.signo_human_readable,
        })
    }
}
