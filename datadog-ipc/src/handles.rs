// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::error::Error;

use super::platform::PlatformHandle;

// Access ability to transport handles between processes
pub trait HandlesTransport {
    type Error: Error;

    /// Move handle out of an object, to send it to remote process
    fn copy_handle<T>(self, handle: PlatformHandle<T>) -> Result<(), Self::Error>;

    /// Fetch handle received from a remote process based on supplied hint
    fn provide_handle<T>(self, hint: &PlatformHandle<T>) -> Result<PlatformHandle<T>, Self::Error>;
}

/// TransferHandles allows moving PlatformHandles from
pub trait TransferHandles {
    fn copy_handles<Transport: HandlesTransport>(
        &self,
        transport: Transport,
    ) -> Result<(), Transport::Error>;

    fn receive_handles<Transport: HandlesTransport>(
        &mut self,
        transport: Transport,
    ) -> Result<(), Transport::Error>;
}

impl<T: TransferHandles> TransferHandles for &T {
    fn copy_handles<Transport: HandlesTransport>(
        &self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        (*self).copy_handles(transport)
    }

    fn receive_handles<Transport: HandlesTransport>(
        &mut self,
        _transport: Transport,
    ) -> Result<(), Transport::Error> {
        unreachable!("receive handles should never be called on a reference (only mut reference)")
    }
}

/// Collects raw file descriptors to be sent via `SCM_RIGHTS` alongside a message.
#[cfg(unix)]
pub struct FdSink(Vec<std::os::unix::io::RawFd>);

#[cfg(unix)]
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

#[cfg(unix)]
impl Default for FdSink {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(unix)]
impl HandlesTransport for &mut FdSink {
    type Error = std::convert::Infallible;

    fn copy_handle<T>(self, handle: super::platform::PlatformHandle<T>) -> Result<(), Self::Error> {
        if let Some(owned) = &handle.inner {
            use std::os::unix::io::AsRawFd;
            self.0.push(owned.as_raw_fd());
        }
        Ok(())
    }

    fn provide_handle<T>(
        self,
        _hint: &super::platform::PlatformHandle<T>,
    ) -> Result<super::platform::PlatformHandle<T>, Self::Error> {
        unreachable!("FdSink::provide_handle should never be called")
    }
}

/// Distributes received `SCM_RIGHTS` file descriptors into `PlatformHandle` fields.
///
/// Created fresh for each received message — no global fd queue, no fd stranding.
#[cfg(unix)]
pub struct FdSource(std::collections::VecDeque<io_lifetimes::OwnedFd>);

#[cfg(unix)]
impl FdSource {
    pub fn new(fds: Vec<io_lifetimes::OwnedFd>) -> Self {
        FdSource(fds.into_iter().collect())
    }
}

#[cfg(unix)]
impl HandlesTransport for &mut FdSource {
    type Error = std::io::Error;

    fn copy_handle<T>(
        self,
        _handle: super::platform::PlatformHandle<T>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn provide_handle<T>(
        self,
        _hint: &super::platform::PlatformHandle<T>,
    ) -> Result<super::platform::PlatformHandle<T>, Self::Error> {
        use std::os::unix::io::{FromRawFd, IntoRawFd};
        let fd = self.0.pop_front().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "no more SCM_RIGHTS file descriptors available",
            )
        })?;
        Ok(unsafe { super::platform::PlatformHandle::from_raw_fd(fd.into_raw_fd()) })
    }
}

impl<T, E> TransferHandles for Result<T, E>
where
    T: TransferHandles,
{
    fn copy_handles<Transport>(&self, transport: Transport) -> Result<(), Transport::Error>
    where
        Transport: HandlesTransport,
    {
        match self {
            Ok(i) => i.copy_handles(transport),
            Err(_) => Ok(()),
        }
    }

    fn receive_handles<Transport>(
        &mut self,
        transport: Transport,
    ) -> Result<(), Transport::Error>
    where
        Transport: HandlesTransport,
    {
        match self {
            Ok(i) => i.receive_handles(transport),
            Err(_) => Ok(()),
        }
    }
}

impl<T> TransferHandles for Option<T>
where
    T: TransferHandles,
{
    fn copy_handles<Transport: HandlesTransport>(
        &self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        match self {
            Some(s) => s.copy_handles(transport),
            None => Ok(()),
        }
    }

    fn receive_handles<Transport: HandlesTransport>(
        &mut self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        match self {
            Some(s) => s.receive_handles(transport),
            #[allow(clippy::todo)]
            None => todo!(),
        }
    }
}
