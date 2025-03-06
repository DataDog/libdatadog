// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use ddcommon_ffi::{slice::AsBytes, CharSlice};
#[repr(C)]
pub struct Span<'a> {
    pub id: CharSlice<'a>,
    pub thread_name: CharSlice<'a>,
}

impl<'a> TryFrom<Span<'a>> for datadog_crashtracker::Span {
    type Error = anyhow::Error;
    fn try_from(value: Span<'a>) -> anyhow::Result<Self> {
        Ok(Self {
            id: value
                .id
                .try_to_string_option()?
                .context("id cannot be empty")?,
            thread_name: value.thread_name.try_to_string_option()?,
        })
    }
}
