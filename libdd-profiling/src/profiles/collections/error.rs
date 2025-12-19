// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_common::error::FfiSafeErrorMessage;
use std::ffi::CStr;
use std::fmt::{Display, Formatter};

#[repr(C)]
#[derive(Debug)]
pub enum SetError {
    InvalidArgument,
    OutOfMemory,
    ReferenceCountOverflow,
}

impl From<libdd_alloc::AllocError> for SetError {
    fn from(_: libdd_alloc::AllocError) -> Self {
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

unsafe impl FfiSafeErrorMessage for SetError {
    fn as_ffi_str(&self) -> &'static CStr {
        match self {
            SetError::InvalidArgument => c"set error: invalid argument",
            SetError::OutOfMemory => c"set error: out of memory",
            SetError::ReferenceCountOverflow => c"set error: reference count overflow",
        }
    }

    fn as_rust_str(&self) -> &'static str {
        // todo: MSRV 1.87: use str::from_utf8_unchecked
        match self {
            SetError::InvalidArgument => "set error: invalid argument",
            SetError::OutOfMemory => "set error: out of memory",
            SetError::ReferenceCountOverflow => "set error: reference count overflow",
        }
    }
}

impl Display for SetError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.as_rust_str().fmt(f)
    }
}

impl core::error::Error for SetError {}
