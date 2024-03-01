// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::exporter::{self, Endpoint};
pub use datadog_crashtracker::{CrashtrackerResolveFrames, ProfilingOpTypes};
use ddcommon::tag::Tag;
use ddcommon_ffi::slice::{AsBytes, CharSlice};
use ddcommon_ffi::Error;
use std::ops::Not;

#[repr(C)]
pub struct CrashtrackerConfiguration<'a> {
    /// Should the crashtracker attempt to collect a stacktrace for the crash
    pub collect_stacktrace: bool,
    pub create_alt_stack: bool,
    /// The endpoint to send the crash repor to (can be a file://)
    pub endpoint: Endpoint<'a>,
    /// Optional filename to forward stderr to (useful for logging/debugging)
    pub optional_stderr_filename: CharSlice<'a>,
    /// Optional filename to forward stdout to (useful for logging/debugging)
    pub optional_stdout_filename: CharSlice<'a>,
    pub path_to_receiver_binary: CharSlice<'a>,
    /// Whether/when we should attempt to resolve frames
    pub resolve_frames: CrashtrackerResolveFrames,
}

impl<'a> TryFrom<CrashtrackerConfiguration<'a>>
    for datadog_crashtracker::CrashtrackerConfiguration
{
    type Error = anyhow::Error;
    fn try_from(value: CrashtrackerConfiguration<'a>) -> anyhow::Result<Self> {
        fn option_from_char_slice(s: CharSlice) -> anyhow::Result<Option<String>> {
            let s = s.try_to_utf8()?.to_string();
            Ok(s.is_empty().not().then_some(s))
        }
        let collect_stacktrace = value.collect_stacktrace;
        let create_alt_stack = value.create_alt_stack;
        let endpoint = unsafe { Some(exporter::try_to_endpoint(value.endpoint)?) };
        let path_to_receiver_binary = value.path_to_receiver_binary.try_to_utf8()?.to_string();
        let resolve_frames = value.resolve_frames;
        let stderr_filename = option_from_char_slice(value.optional_stderr_filename)?;
        let stdout_filename = option_from_char_slice(value.optional_stdout_filename)?;

        Self::new(
            collect_stacktrace,
            create_alt_stack,
            endpoint,
            path_to_receiver_binary,
            resolve_frames,
            stderr_filename,
            stdout_filename,
        )
    }
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

/// Represents a CrashInfo. Do not access its member for any reason, only use
/// the C API functions on this struct.
#[repr(C)]
pub struct CrashInfo {
    // This may be null, but if not it will point to a valid CrashInfo.
    inner: *mut datadog_crashtracker::CrashInfo,
}

impl CrashInfo {
    pub(super) fn new(crash_info: datadog_crashtracker::CrashInfo) -> Self {
        CrashInfo {
            inner: Box::into_raw(Box::new(crash_info)),
        }
    }

    fn take(&mut self) -> Option<Box<datadog_crashtracker::CrashInfo>> {
        // Leaving a null will help with double-free issues that can
        // arise in C. Of course, it's best to never get there in the
        // first place!
        let raw = std::mem::replace(&mut self.inner, std::ptr::null_mut());

        if raw.is_null() {
            None
        } else {
            Some(unsafe { Box::from_raw(raw) })
        }
    }
}

impl Drop for CrashInfo {
    fn drop(&mut self) {
        drop(self.take())
    }
}

pub(crate) unsafe fn crashinfo_ptr_to_inner<'a>(
    crashinfo_ptr: *mut CrashInfo,
) -> anyhow::Result<&'a mut datadog_crashtracker::CrashInfo> {
    match crashinfo_ptr.as_mut() {
        None => anyhow::bail!("crashinfo pointer was null"),
        Some(inner_ptr) => match inner_ptr.inner.as_mut() {
            Some(crashinfo) => Ok(crashinfo),
            None => anyhow::bail!("crashinfo's inner pointer was null (indicates use-after-free)"),
        },
    }
}

/// Returned by [ddog_prof_Profile_new].
#[repr(C)]
pub enum CrashInfoNewResult {
    Ok(CrashInfo),
    #[allow(dead_code)]
    Err(Error),
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
