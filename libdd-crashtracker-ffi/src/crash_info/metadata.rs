// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_common::tag::Tag;
use libdd_common_ffi::{slice::AsBytes, CharSlice};

#[repr(C)]
pub struct Metadata<'a> {
    pub library_name: CharSlice<'a>,
    pub library_version: CharSlice<'a>,
    pub family: CharSlice<'a>,
    /// Should include "service", "environment", etc
    pub tags: Option<&'a libdd_common_ffi::Vec<Tag>>,
}

impl<'a> TryFrom<Metadata<'a>> for libdd_crashtracker::Metadata {
    type Error = anyhow::Error;
    fn try_from(value: Metadata<'a>) -> anyhow::Result<Self> {
        let library_name = value.library_name.try_to_string()?;
        let library_version = value.library_version.try_to_string()?;
        let family = value.family.try_to_string()?;
        let tags = if let Some(tags) = value.tags {
            tags.into_iter().map(|t| t.to_string()).collect()
        } else {
            vec![]
        };
        Ok(Self {
            library_name,
            library_version,
            family,
            tags,
        })
    }
}
