// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_crashtracker::{SiCodes, SignalNames};
use libdd_common_ffi::{slice::AsBytes, CharSlice};

#[repr(C)]
pub struct SigInfo<'a> {
    pub addr: CharSlice<'a>,
    pub code: libc::c_int,
    pub code_human_readable: SiCodes,
    pub signo: libc::c_int,
    pub signo_human_readable: SignalNames,
}

impl<'a> TryFrom<SigInfo<'a>> for datadog_crashtracker::SigInfo {
    type Error = anyhow::Error;
    fn try_from(value: SigInfo<'a>) -> anyhow::Result<Self> {
        Ok(Self {
            si_addr: value.addr.try_to_string_option()?,
            si_code: value.code,
            si_code_human_readable: value.code_human_readable,
            si_signo: value.signo,
            si_signo_human_readable: value.signo_human_readable,
        })
    }
}
