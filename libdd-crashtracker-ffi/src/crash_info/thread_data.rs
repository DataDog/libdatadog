// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use libdd_common_ffi::{slice::AsBytes, CharSlice, Handle, ToInner};

use libdd_crashtracker::StackTrace;

#[repr(C)]
pub struct ThreadData<'a> {
    pub crashed: bool,
    pub name: CharSlice<'a>,
    pub stack: Handle<StackTrace>,
    pub state: CharSlice<'a>,
}

impl<'a> TryFrom<ThreadData<'a>> for libdd_crashtracker::ThreadData {
    type Error = anyhow::Error;
    fn try_from(mut value: ThreadData<'a>) -> anyhow::Result<Self> {
        let crashed = value.crashed;
        let name = value
            .name
            .try_to_string_option()?
            .context("Name cannot be empty")?;
        // Safety: Handles should only be created using functions that leave them in a valid state.
        let stack = unsafe { *value.stack.take()? };
        let state = value.state.try_to_string_option()?;

        Ok(Self {
            crashed,
            name,
            stack,
            state,
        })
    }
}
