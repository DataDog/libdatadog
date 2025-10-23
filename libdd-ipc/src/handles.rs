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

mod transport_impls {
    use super::{HandlesTransport, TransferHandles};

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

    use tarpc::{ClientMessage, Request, Response};

    impl<T: TransferHandles> TransferHandles for Response<T> {
        fn copy_handles<Transport: HandlesTransport>(
            &self,
            transport: Transport,
        ) -> Result<(), Transport::Error> {
            self.message.copy_handles(transport)
        }

        fn receive_handles<Transport: HandlesTransport>(
            &mut self,
            transport: Transport,
        ) -> Result<(), Transport::Error> {
            self.message.receive_handles(transport)
        }
    }

    impl<T> TransferHandles for ClientMessage<T>
    where
        T: TransferHandles,
    {
        fn copy_handles<M>(&self, mover: M) -> Result<(), M::Error>
        where
            M: HandlesTransport,
        {
            match self {
                ClientMessage::Request(r) => r.copy_handles(mover),
                ClientMessage::Cancel {
                    trace_context: _,
                    request_id: _,
                } => Ok(()),
                _ => Ok(()),
            }
        }
        fn receive_handles<P>(&mut self, provider: P) -> Result<(), P::Error>
        where
            P: HandlesTransport,
        {
            match self {
                ClientMessage::Request(r) => r.receive_handles(provider),
                ClientMessage::Cancel {
                    trace_context: _,
                    request_id: _,
                } => Ok(()),
                _ => Ok(()),
            }
        }
    }

    impl<T: TransferHandles> TransferHandles for Request<T> {
        fn receive_handles<P>(&mut self, provider: P) -> Result<(), P::Error>
        where
            P: HandlesTransport,
        {
            self.message.receive_handles(provider)
        }

        fn copy_handles<M>(&self, mover: M) -> Result<(), M::Error>
        where
            M: HandlesTransport,
        {
            self.message.copy_handles(mover)
        }
    }
}
