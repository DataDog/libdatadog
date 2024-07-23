// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub use datadog_crashtracker::ProfilingOpTypes;
use ddcommon::tag::Tag;
use ddcommon_ffi::slice::{AsBytes, CharSlice};
use ddcommon_ffi::{Error, StringWrapper};
use std::ops::Not;

pub fn option_from_char_slice(s: CharSlice) -> anyhow::Result<Option<String>> {
    let s = s.try_to_utf8()?.to_string();
    Ok(s.is_empty().not().then_some(s))
}

#[repr(C)]
pub struct CrashtrackerMetadata<'a> {
    pub profiling_library_name: CharSlice<'a>,
    pub profiling_library_version: CharSlice<'a>,
    pub family: CharSlice<'a>,
    /// Should include "service", "environment", etc
    pub tags: Option<&'a ddcommon_ffi::Vec<Tag>>,
}

impl<'a> TryFrom<CrashtrackerMetadata<'a>> for datadog_crashtracker::CrashtrackerMetadata {
    type Error = anyhow::Error;
    fn try_from(value: CrashtrackerMetadata<'a>) -> anyhow::Result<Self> {
        let profiling_library_name = value.profiling_library_name.try_to_utf8()?.to_string();
        let profiling_library_version = value.profiling_library_version.try_to_utf8()?.to_string();
        let family = value.family.try_to_utf8()?.to_string();
        let tags = value
            .tags
            .map(|tags| tags.iter().cloned().collect())
            .unwrap_or_default();
        Ok(Self::new(
            profiling_library_name,
            profiling_library_version,
            family,
            tags,
        ))
    }
}

/// Returned by [ddog_prof_Profile_new].
#[repr(C)]
pub enum StringWrapperResult {
    Ok(StringWrapper),
    #[allow(dead_code)]
    Err(Error),
}

// Useful for testing
impl StringWrapperResult {
    pub fn unwrap(self) -> StringWrapper {
        match self {
            StringWrapperResult::Ok(s) => s,
            StringWrapperResult::Err(e) => panic!("{e}"),
        }
    }
}

impl From<anyhow::Result<String>> for StringWrapperResult {
    fn from(value: anyhow::Result<String>) -> Self {
        match value {
            Ok(x) => Self::Ok(x.into()),
            Err(err) => Self::Err(err.into()),
        }
    }
}

impl From<String> for StringWrapperResult {
    fn from(value: String) -> Self {
        Self::Ok(value.into())
    }
}

#[repr(C)]
pub enum CrashtrackerUsizeResult {
    Ok(usize),
    #[allow(dead_code)]
    Err(Error),
}

impl From<anyhow::Result<usize>> for CrashtrackerUsizeResult {
    fn from(value: anyhow::Result<usize>) -> Self {
        match value {
            Ok(x) => Self::Ok(x),
            Err(err) => Self::Err(err.into()),
        }
    }
}

#[repr(C)]
pub enum CrashtrackerGetCountersResult {
    Ok([i64; ProfilingOpTypes::SIZE as usize]),
    #[allow(dead_code)]
    Err(Error),
}

impl From<anyhow::Result<[i64; ProfilingOpTypes::SIZE as usize]>>
    for CrashtrackerGetCountersResult
{
    fn from(value: anyhow::Result<[i64; ProfilingOpTypes::SIZE as usize]>) -> Self {
        match value {
            Ok(x) => Self::Ok(x),
            Err(err) => Self::Err(err.into()),
        }
    }
}

/// A generic result type for when a crashtracking operation may fail,
/// but there's nothing to return in the case of success.
#[repr(C)]
pub enum CrashtrackerResult {
    Ok(
        /// Do not use the value of Ok. This value only exists to overcome
        /// Rust -> C code generation.
        bool,
    ),
    Err(Error),
}

impl From<anyhow::Result<()>> for CrashtrackerResult {
    fn from(value: anyhow::Result<()>) -> Self {
        match value {
            Ok(_) => Self::Ok(true),
            Err(err) => Self::Err(err.into()),
        }
    }
}
