// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::error::Error;

use super::platform::PlatformHandle;

// Access ability to transport handles between processes
pub trait HandlesTransport {
    type Error: Error;

    /// Move handle out of an object, to send it to remote process
    fn move_handle<T>(self, handle: PlatformHandle<T>) -> Result<(), Self::Error>;

    /// Fetch handle received from a remote process based on supplied hint
    fn provide_handle<T>(self, hint: &PlatformHandle<T>) -> Result<PlatformHandle<T>, Self::Error>;
}

/// TransferHandles allows moving PlatformHandles from
pub trait TransferHandles {
    fn move_handles<Transport: HandlesTransport>(
        &self,
        transport: Transport,
    ) -> Result<(), Transport::Error>;

    fn receive_handles<Transport: HandlesTransport>(
        &mut self,
        transport: Transport,
    ) -> Result<(), Transport::Error>;
}

mod transport_impls {
    use super::{HandlesTransport, TransferHandles};

    impl<T, E> TransferHandles for Result<T, E>
    where
        T: TransferHandles,
    {
        fn move_handles<Transport>(&self, transport: Transport) -> Result<(), Transport::Error>
        where
            Transport: HandlesTransport,
        {
            match self {
                Ok(i) => i.move_handles(transport),
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
        fn move_handles<Transport: HandlesTransport>(
            &self,
            transport: Transport,
        ) -> Result<(), Transport::Error> {
            match self {
                Some(s) => s.move_handles(transport),
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
        fn move_handles<Transport: HandlesTransport>(
            &self,
            transport: Transport,
        ) -> Result<(), Transport::Error> {
            self.message.move_handles(transport)
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
        fn move_handles<M>(&self, mover: M) -> Result<(), M::Error>
        where
            M: HandlesTransport,
        {
            match self {
                ClientMessage::Request(r) => r.move_handles(mover),
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

        fn move_handles<M>(&self, mover: M) -> Result<(), M::Error>
        where
            M: HandlesTransport,
        {
            self.message.move_handles(mover)
        }
    }
}
