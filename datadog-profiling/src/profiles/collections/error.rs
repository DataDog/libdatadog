// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[repr(C)]
#[derive(Debug)]
pub enum SetError {
    InvalidArgument,
    OutOfMemory,
    ReferenceCountOverflow,
}

impl core::fmt::Display for SetError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SetError::InvalidArgument => "set error: invalid argument",
            SetError::OutOfMemory => "set error: out of memory",
            SetError::ReferenceCountOverflow => "set error: reference count overflow",
        }
        .fmt(f)
    }
}

impl core::error::Error for SetError {}

impl From<datadog_alloc::AllocError> for SetError {
    fn from(_: datadog_alloc::AllocError) -> Self {
        SetError::OutOfMemory
    }
}

impl From<std::collections::TryReserveError> for SetError {
    fn from(_: std::collections::TryReserveError) -> Self {
        SetError::OutOfMemory
    }
}

impl From<hashbrown::TryReserveError> for SetError {
    fn from(_: hashbrown::TryReserveError) -> Self {
        SetError::OutOfMemory
    }
}

impl From<super::ArcOverflow> for SetError {
    fn from(_: super::ArcOverflow) -> Self {
        SetError::ReferenceCountOverflow
    }
}
