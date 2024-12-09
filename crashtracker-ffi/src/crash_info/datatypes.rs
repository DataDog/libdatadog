// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon_ffi::{
    slice::{AsBytes, ByteSlice},
    CharSlice, Slice,
};

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
        let filename = value.filename.try_to_string_option()?;
        let lineno = (&value.lineno).into();
        let name = value.name.try_to_string_option()?;
        Ok(Self {
            colno,
            filename,
            lineno,
            name,
        })
    }
}

#[repr(C)]
pub struct StackFrameOld<'a> {
    build_id: CharSlice<'a>,
    ip: usize,
    module_base_address: usize,
    names: Slice<'a, StackFrameNames<'a>>,
    normalized_ip: NormalizedAddress<'a>,
    sp: usize,
    symbol_address: usize,
}

impl<'a> TryFrom<&StackFrameOld<'a>> for datadog_crashtracker::StackFrame {
    type Error = anyhow::Error;

    fn try_from(value: &StackFrameOld<'a>) -> Result<Self, Self::Error> {
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
