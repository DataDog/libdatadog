// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Error {
    OutOfMemory,
    StorageFull,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Error::OutOfMemory => "out of memory",
            Error::StorageFull => "storage full",
        };
        std::fmt::Display::fmt(msg, f)
    }
}

impl core::error::Error for Error {}

impl From<datadog_alloc::AllocError> for Error {
    fn from(_: datadog_alloc::AllocError) -> Error {
        Error::OutOfMemory
    }
}

impl From<std::collections::TryReserveError> for Error {
    fn from(_: std::collections::TryReserveError) -> Error {
        Error::OutOfMemory
    }
}

impl From<indexmap::TryReserveError> for Error {
    fn from(_: indexmap::TryReserveError) -> Error {
        Error::OutOfMemory
    }
}
