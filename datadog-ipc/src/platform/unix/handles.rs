// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::handles::HandlesTransport;
use crate::platform::PlatformHandle;
use io_lifetimes::OwnedFd;
use std::collections::VecDeque;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd};

/// Collects raw file descriptors to be sent via `SCM_RIGHTS` alongside a message.
pub struct FdSink(Vec<std::os::unix::io::RawFd>);

impl FdSink {
    pub fn new() -> Self {
        FdSink(Vec::new())
    }

    pub fn fds(&self) -> &[std::os::unix::io::RawFd] {
        &self.0
    }

    pub fn into_fds(self) -> Vec<std::os::unix::io::RawFd> {
        self.0
    }
}

impl Default for FdSink {
    fn default() -> Self {
        Self::new()
    }
}

impl HandlesTransport for &mut FdSink {
    type Error = std::convert::Infallible;

    fn copy_handle<T>(self, handle: PlatformHandle<T>) -> Result<(), Self::Error> {
        if let Some(owned) = &handle.inner {
            self.0.push(owned.as_raw_fd());
        }
        Ok(())
    }

    fn provide_handle<T>(
        self,
        _hint: &PlatformHandle<T>,
    ) -> Result<PlatformHandle<T>, Self::Error> {
        unreachable!("FdSink::provide_handle should never be called")
    }
}

/// Distributes received `SCM_RIGHTS` file descriptors into `PlatformHandle` fields.
///
/// Every message should have its own FdSource.
pub struct FdSource(VecDeque<OwnedFd>);

impl FdSource {
    pub fn new(fds: Vec<OwnedFd>) -> Self {
        FdSource(fds.into_iter().collect())
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
        let fd = self.0.pop_front().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "no more SCM_RIGHTS file descriptors available",
            )
        })?;
        Ok(unsafe { PlatformHandle::from_raw_fd(fd.into_raw_fd()) })
    }
}
