// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::handles::HandlesTransport;
use crate::platform::PlatformHandle;
use std::collections::VecDeque;
use std::os::windows::io::{FromRawHandle, IntoRawHandle, OwnedHandle};

/// No-op sink — Windows handles are transferred in-band via message suffix, not out-of-band.
pub struct FdSink;

impl FdSink {
    pub fn new() -> Self {
        FdSink
    }

    pub fn into_fds(self) -> Vec<std::os::windows::io::RawHandle> {
        Vec::new()
    }
}

impl Default for FdSink {
    fn default() -> Self {
        Self::new()
    }
}

impl HandlesTransport for &mut FdSink {
    type Error = std::convert::Infallible;

    fn copy_handle<T>(self, _handle: PlatformHandle<T>) -> Result<(), Self::Error> {
        Ok(())
    }

    fn provide_handle<T>(
        self,
        _hint: &PlatformHandle<T>,
    ) -> Result<PlatformHandle<T>, Self::Error> {
        unreachable!("FdSink::provide_handle should never be called")
    }
}

/// Distributes handles extracted from the in-band wire suffix into `PlatformHandle` fields.
pub struct FdSource(VecDeque<OwnedHandle>);

impl FdSource {
    pub fn new(handles: Vec<OwnedHandle>) -> Self {
        FdSource(handles.into_iter().collect())
    }
}

impl HandlesTransport for &mut FdSource {
    type Error = std::io::Error;

    fn copy_handle<T>(self, _handle: PlatformHandle<T>) -> Result<(), Self::Error> {
        Ok(())
    }

    fn provide_handle<T>(
        self,
        _hint: &PlatformHandle<T>,
    ) -> Result<PlatformHandle<T>, Self::Error> {
        let handle = self.0.pop_front().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "no more handles available",
            )
        })?;
        Ok(unsafe { PlatformHandle::from_raw_handle(handle.into_raw_handle()) })
    }
}
