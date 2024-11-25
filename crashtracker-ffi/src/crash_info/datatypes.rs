// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::option_from_char_slice;
use ddcommon::tag::Tag;
use ddcommon_ffi::{
    slice::{AsBytes, ByteSlice},
    CharSlice, Error, Slice,
};

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

#[repr(C)]
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum NormalizedAddressTypes {
    // Make None 0 so that default construction gives none
    None = 0,
    Elf,
    Pdb,
}

#[repr(C)]
pub struct NormalizedAddress<'a> {
    file_offset: u64,
    build_id: ByteSlice<'a>,
    age: u64,
    path: CharSlice<'a>,
    typ: NormalizedAddressTypes,
}

impl<'a> TryFrom<NormalizedAddress<'a>> for Option<datadog_crashtracker::NormalizedAddress> {
    type Error = anyhow::Error;

    fn try_from(value: NormalizedAddress<'a>) -> Result<Self, Self::Error> {
        Self::try_from(&value)
    }
}

impl<'a> TryFrom<&NormalizedAddress<'a>> for Option<datadog_crashtracker::NormalizedAddress> {
    type Error = anyhow::Error;

    fn try_from(value: &NormalizedAddress<'a>) -> Result<Self, Self::Error> {
        if value.typ == NormalizedAddressTypes::None {
            Ok(None)
        } else {
            Ok(Some(value.try_into()?))
        }
    }
}

impl<'a> TryFrom<NormalizedAddress<'a>> for datadog_crashtracker::NormalizedAddress {
    type Error = anyhow::Error;

    fn try_from(value: NormalizedAddress<'a>) -> Result<Self, Self::Error> {
        Self::try_from(&value)
    }
}

impl<'a> TryFrom<&NormalizedAddress<'a>> for datadog_crashtracker::NormalizedAddress {
    type Error = anyhow::Error;

    fn try_from(value: &NormalizedAddress<'a>) -> Result<Self, Self::Error> {
        let to_opt_bytes = |v: ByteSlice| {
            if v.is_empty() {
                None
            } else {
                Some(Vec::from(v.as_bytes()))
            }
        };
        match &value.typ {
            NormalizedAddressTypes::Elf => {
                let build_id = to_opt_bytes(value.build_id);
                let path = value.path.try_to_utf8()?.into();
                let meta = datadog_crashtracker::NormalizedAddressMeta::Elf { build_id, path };
                Ok(Self {
                    file_offset: value.file_offset,
                    meta,
                })
            }
            NormalizedAddressTypes::Pdb => {
                let guid = to_opt_bytes(value.build_id);
                let path = value.path.try_to_utf8()?.into();
                let age = value.age;
                let meta = datadog_crashtracker::NormalizedAddressMeta::Pdb { path, guid, age };
                Ok(Self {
                    file_offset: value.file_offset,
                    meta,
                })
            }
            _ => anyhow::bail!("Unsupported normalized address type {:?}", value.typ),
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
    build_id: CharSlice<'a>,
    ip: usize,
    module_base_address: usize,
    names: Slice<'a, StackFrameNames<'a>>,
    normalized_ip: NormalizedAddress<'a>,
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
        let normalized_ip = (&value.normalized_ip).try_into()?;
        let sp = to_hex(value.sp);
        let symbol_address = to_hex(value.symbol_address);
        Ok(Self {
            ip,
            module_base_address,
            names,
            normalized_ip,
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
        let faulting_address = None; // TODO: Expose this to FFI
        Ok(Self {
            signum,
            signame,
            faulting_address,
        })
    }
}

#[repr(C)]
pub struct ProcInfo {
    pub pid: u32,
}

impl TryFrom<ProcInfo> for datadog_crashtracker::ProcessInfo {
    type Error = anyhow::Error;

    fn try_from(value: ProcInfo) -> anyhow::Result<Self> {
        let pid = value.pid;
        Ok(Self { pid })
    }
}

#[repr(C)]
pub struct Metadata<'a> {
    pub library_name: CharSlice<'a>,
    pub library_version: CharSlice<'a>,
    pub family: CharSlice<'a>,
    /// Should include "service", "environment", etc
    pub tags: Option<&'a ddcommon_ffi::Vec<Tag>>,
}

impl<'a> TryFrom<Metadata<'a>> for datadog_crashtracker::CrashtrackerMetadata {
    type Error = anyhow::Error;
    fn try_from(value: Metadata<'a>) -> anyhow::Result<Self> {
        let library_name = value.library_name.try_to_utf8()?.to_string();
        let library_version = value.library_version.try_to_utf8()?.to_string();
        let family = value.family.try_to_utf8()?.to_string();
        let tags = value
            .tags
            .map(|tags| tags.iter().cloned().collect())
            .unwrap_or_default();
        Ok(Self::new(library_name, library_version, family, tags))
    }
}
