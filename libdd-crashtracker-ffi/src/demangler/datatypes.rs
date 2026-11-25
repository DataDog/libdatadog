// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[repr(C)]
pub enum DemangleOptions {
    Complete,
    NameOnly,
}

impl From<DemangleOptions> for u32 {
    fn from(value: DemangleOptions) -> u32 {
        value as u32
    }
}

impl TryFrom<u32> for DemangleOptions {
    type Error = u32;

    fn try_from(value: u32) -> core::result::Result<Self, Self::Error> {
        Ok(match value {
            v if v == u32::from(Self::Complete) => Self::Complete,
            v if v == u32::from(Self::NameOnly) => Self::NameOnly,
            v => return Err(v),
        })
    }
}
