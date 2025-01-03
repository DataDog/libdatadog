// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon_ffi::{slice::AsBytes, CharSlice};

#[repr(C)]
pub struct OsInfo<'a> {
    pub architecture: CharSlice<'a>,
    pub bitness: CharSlice<'a>,
    pub os_type: CharSlice<'a>,
    pub version: CharSlice<'a>,
}

impl<'a> TryFrom<OsInfo<'a>> for datadog_crashtracker::rfc5_crash_info::OsInfo {
    type Error = anyhow::Error;
    fn try_from(value: OsInfo<'a>) -> anyhow::Result<Self> {
        let unknown = || "unknown".to_string();
        let architecture = value
            .architecture
            .try_to_string_option()?
            .unwrap_or_else(unknown);
        let bitness = value
            .bitness
            .try_to_string_option()?
            .unwrap_or_else(unknown);
        let os_type = value
            .os_type
            .try_to_string_option()?
            .unwrap_or_else(unknown);
        let version = value
            .version
            .try_to_string_option()?
            .unwrap_or_else(unknown);
        Ok(Self {
            architecture,
            bitness,
            os_type,
            version,
        })
    }
}
