// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[repr(C)]
#[derive(Debug, thiserror::Error)]
pub enum SetError {
    #[error("set error: invalid argument")]
    InvalidArgument,
    #[error("set error: out of memory")]
    OutOfMemory,
    #[error("set error: reference count overflow")]
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
