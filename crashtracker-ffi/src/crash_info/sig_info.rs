// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_crashtracker::rfc5_crash_info::{SiCodes, SignalNames};
use ddcommon_ffi::{slice::AsBytes, CharSlice};

#[repr(C)]
pub struct SigInfo<'a> {
    pub si_addr: CharSlice<'a>,
    pub si_code: libc::c_int,
    pub si_code_human_readable: SiCodes,
    pub si_signo: libc::c_int,
    pub si_signo_human_readable: SignalNames,
}

impl<'a> TryFrom<SigInfo<'a>> for datadog_crashtracker::rfc5_crash_info::SigInfo {
    type Error = anyhow::Error;
    fn try_from(value: SigInfo<'a>) -> anyhow::Result<Self> {
        Ok(Self {
            si_addr: value.si_addr.try_to_string_option()?,
            si_code: value.si_code,
            si_code_human_readable: value.si_code_human_readable,
            si_signo: value.si_signo,
            si_signo_human_readable: value.si_signo_human_readable,
        })
    }
}
