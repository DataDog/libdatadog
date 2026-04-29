// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::platform::PlatformHandle;

// Access ability to transport handles between processes
pub trait HandlesTransport {
    type Error: std::error::Error;

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

pub use crate::platform::{FdSink, FdSource};

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

    fn receive_handles<Transport>(&mut self, transport: Transport) -> Result<(), Transport::Error>
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
