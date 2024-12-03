// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use ddcommon_ffi::{slice::AsBytes, CharSlice};

use super::StackTrace;

#[repr(C)]
pub struct ThreadData<'a> {
    pub crashed: bool,
    pub name: CharSlice<'a>,
    pub stack: StackTrace,
    pub state: CharSlice<'a>,
}

impl<'a> TryFrom<ThreadData<'a>> for datadog_crashtracker::rfc5_crash_info::ThreadData {
    type Error = anyhow::Error;
    fn try_from(mut value: ThreadData<'a>) -> anyhow::Result<Self> {
        let crashed = value.crashed;
        let name = value
            .name
            .try_to_string_option()?
            .context("Name cannot be empty")?;
        let stack = *value
            .stack
            .take()
            .context("missing stack, use after free?")?;
        let state = value.state.try_to_string_option()?;

        Ok(Self {
            crashed,
            name,
            stack,
            state,
        })
    }
}
