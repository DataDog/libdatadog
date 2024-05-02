// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::exporter::{self, ProfilingEndpoint};
pub use datadog_crashtracker::{ProfilingOpTypes, StacktraceCollection};
use ddcommon::tag::Tag;
use ddcommon_ffi::slice::{AsBytes, CharSlice};
use ddcommon_ffi::{Error, Slice, StringWrapper};
use std::ops::Not;
use std::time::Duration;

#[repr(C)]
pub struct EnvVar<'a> {
    key: CharSlice<'a>,
    val: CharSlice<'a>,
}

#[repr(C)]
pub struct CrashtrackerReceiverConfig<'a> {
    pub args: Slice<'a, CharSlice<'a>>,
    pub env: Slice<'a, EnvVar<'a>>,
    pub path_to_receiver_binary: CharSlice<'a>,
    /// Optional filename to forward stderr to (useful for logging/debugging)
    pub optional_stderr_filename: CharSlice<'a>,
    /// Optional filename to forward stdout to (useful for logging/debugging)
    pub optional_stdout_filename: CharSlice<'a>,
}

#[repr(C)]
pub struct CrashtrackerConfiguration<'a> {
    pub additional_files: Slice<'a, CharSlice<'a>>,
    pub create_alt_stack: bool,
    /// The endpoint to send the crash report to (can be a file://)
    pub endpoint: ProfilingEndpoint<'a>,
    pub resolve_frames: StacktraceCollection,
    pub timeout_secs: u64,
}

pub fn option_from_char_slice(s: CharSlice) -> anyhow::Result<Option<String>> {
    let s = s.try_to_utf8()?.to_string();
    Ok(s.is_empty().not().then_some(s))
}

impl<'a> TryFrom<CrashtrackerReceiverConfig<'a>>
    for datadog_crashtracker::CrashtrackerReceiverConfig
{
    type Error = anyhow::Error;
    fn try_from(value: CrashtrackerReceiverConfig<'a>) -> anyhow::Result<Self> {
        let args = {
            let mut vec = Vec::with_capacity(value.args.len());
            for x in value.args.iter() {
                vec.push(x.try_to_utf8()?.to_string());
            }
            vec
        };
        let env = {
            let mut vec = Vec::with_capacity(value.env.len());
            for x in value.env.iter() {
                vec.push((
                    x.key.try_to_utf8()?.to_string(),
                    x.val.try_to_utf8()?.to_string(),
                ));
            }
            vec
        };
        let path_to_receiver_binary = value.path_to_receiver_binary.try_to_utf8()?.to_string();
        let stderr_filename = option_from_char_slice(value.optional_stderr_filename)?;
        let stdout_filename = option_from_char_slice(value.optional_stdout_filename)?;
        Self::new(
            args,
            env,
            path_to_receiver_binary,
            stderr_filename,
            stdout_filename,
        )
    }
}

impl<'a> TryFrom<CrashtrackerConfiguration<'a>>
    for datadog_crashtracker::CrashtrackerConfiguration
{
    type Error = anyhow::Error;
    fn try_from(value: CrashtrackerConfiguration<'a>) -> anyhow::Result<Self> {
        let additional_files = {
            let mut vec = Vec::with_capacity(value.additional_files.len());
            for x in value.additional_files.iter() {
                vec.push(x.try_to_utf8()?.to_string());
            }
            vec
        };
        let create_alt_stack = value.create_alt_stack;
        let endpoint = unsafe { Some(exporter::try_to_endpoint(value.endpoint)?) };
        let resolve_frames = value.resolve_frames;
        let timeout = Duration::from_secs(value.timeout_secs);
        Self::new(
            additional_files,
            create_alt_stack,
            endpoint,
            resolve_frames,
            timeout,
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

    pub(super) fn take(&mut self) -> Option<Box<datadog_crashtracker::CrashInfo>> {
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

#[repr(C)]
pub struct StackFrameNames<'a> {
    colno: ddcommon_ffi::Option<u32>,
    filename: CharSlice<'a>,
    lineno: ddcommon_ffi::Option<u32>,
    name: CharSlice<'a>,
}

impl<'a> TryFrom<StackFrameNames<'a>> for datadog_crashtracker::StackFrameNames {
    type Error = anyhow::Error;

    fn try_from(value: StackFrameNames<'a>) -> Result<Self, Self::Error> {
        Self::try_from(&value)
    }
}

impl<'a> TryFrom<&StackFrameNames<'a>> for datadog_crashtracker::StackFrameNames {
    type Error = anyhow::Error;

    fn try_from(value: &StackFrameNames<'a>) -> Result<Self, Self::Error> {
        let colno = (&value.colno).into();
        let filename = option_from_char_slice(value.filename)?;
        let lineno = (&value.lineno).into();
        let name = option_from_char_slice(value.name)?;
        Ok(Self {
            colno,
            filename,
            lineno,
            name,
        })
    }
}

#[repr(C)]
pub struct StackFrame<'a> {
    ip: usize,
    module_base_address: usize,
    names: Slice<'a, StackFrameNames<'a>>,
    sp: usize,
    symbol_address: usize,
}

impl<'a> TryFrom<&StackFrame<'a>> for datadog_crashtracker::StackFrame {
    type Error = anyhow::Error;

    fn try_from(value: &StackFrame<'a>) -> Result<Self, Self::Error> {
        fn to_hex(v: usize) -> Option<String> {
            if v == 0 {
                None
            } else {
                Some(format!("{v:#X}"))
            }
        }

        let ip = to_hex(value.ip);
        let module_base_address = to_hex(value.module_base_address);
        let names = if value.names.is_empty() {
            None
        } else {
            let mut vec = Vec::with_capacity(value.names.len());
            for x in value.names.iter() {
                vec.push(x.try_into()?);
            }
            Some(vec)
        };
        let sp = to_hex(value.sp);
        let symbol_address = to_hex(value.symbol_address);
        Ok(Self {
            ip,
            module_base_address,
            names,
            sp,
            symbol_address,
        })
    }
}

#[repr(C)]
pub struct SigInfo<'a> {
    pub signum: u64,
    pub signame: CharSlice<'a>,
}

impl<'a> TryFrom<SigInfo<'a>> for datadog_crashtracker::SigInfo {
    type Error = anyhow::Error;

    fn try_from(value: SigInfo<'a>) -> Result<Self, Self::Error> {
        let signum = value.signum;
        let signame = option_from_char_slice(value.signame)?;
        Ok(Self { signum, signame })
    }
}
