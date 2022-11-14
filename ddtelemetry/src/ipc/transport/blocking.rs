// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::{
    io::{self, Read, Write},
    mem::MaybeUninit,
    os::unix::net::UnixStream,
    pin::Pin,
    sync::{atomic::AtomicU64, Arc},
    time::{Duration, SystemTime},
};

use bytes::{BufMut, BytesMut};
use serde::{Deserialize, Serialize};
use tarpc::{context, trace, Response};
use tokio_serde::{formats::MessagePack, Deserializer, Serializer};
use tokio_util::codec::{Decoder, Encoder, LengthDelimitedCodec};

use crate::ipc::{
    handles::{HandlesTransport, TransferHandles},
    platform::{Channel, Message},
};

pub struct BlockingTransport<IncomingItem, OutgoingItem> {
    pid: libc::pid_t,
    requests_id: Arc<AtomicU64>,
    transport: FramedBlocking<Response<IncomingItem>, ClientMessage<OutgoingItem>>,
}

impl<IncomingItem, OutgoingItem> Clone for BlockingTransport<IncomingItem, OutgoingItem> {
    fn clone(&self) -> Self {
        Self {
            pid: self.pid,
            requests_id: self.requests_id.clone(),
            transport: self.transport.clone(),
        }
    }
}

impl<IncomingItem, OutgoingItem> From<Channel> for BlockingTransport<IncomingItem, OutgoingItem> {
    fn from(c: Channel) -> Self {
        let pid = unsafe { libc::getpid() };
        BlockingTransport {
            pid,
            requests_id: Arc::from(AtomicU64::new(0)),
            transport: c.into(),
        }
    }
}

impl<IncomingItem, OutgoingItem> From<UnixStream>
    for BlockingTransport<IncomingItem, OutgoingItem>
{
    fn from(s: UnixStream) -> Self {
        let pid = unsafe { libc::getpid() };
        BlockingTransport {
            pid,
            requests_id: Arc::from(AtomicU64::new(0)),
            transport: Channel::from(s).into(),
        }
    }
}

pub struct FramedBlocking<IncomingItem, OutgoingItem> {
    codec: LengthDelimitedCodec,
    read_buffer: BytesMut,
    channel: Channel,
    serde_codec: Pin<Box<MessagePack<Message<IncomingItem>, Message<OutgoingItem>>>>,
}

impl<IncomingItem, OutgoingItem> FramedBlocking<IncomingItem, OutgoingItem>
where
    IncomingItem: for<'de> Deserialize<'de> + TransferHandles,
    OutgoingItem: Serialize + TransferHandles,
{
    pub fn read_item(&mut self) -> Result<IncomingItem, io::Error> {
        let buf = &mut self.read_buffer;
        while buf.has_remaining_mut() {
            buf.reserve(1);
            match self.codec.decode(buf)? {
                Some(frame) => {
                    let message = self.serde_codec.as_mut().deserialize(&frame)?;
                    let item = self.channel.metadata.unwrap_message(message)?;
                    return Ok(item);
                }
                None => {
                    let n = unsafe {
                        let dst = buf.chunk_mut();
                        let dst = &mut *(dst as *mut _ as *mut [MaybeUninit<u8>]);
                        let mut buf_window = tokio::io::ReadBuf::uninit(dst);
                        // this implementation is based on Tokio async read implementation,
                        // it is performing an UB operation by using uninitiallized memory - although in practice its somewhat defined
                        // there are still some unknowns WRT to future behaviors

                        // TODO: make sure this optimization is really needed - once BenchPlatform is connected to libdatadog
                        // benchmark unfilled_mut vs initialize_unfilled - and if the difference is negligible - then lets switch to
                        // implementation that doesn't use UB.
                        let b = &mut *(buf_window.unfilled_mut()
                            as *mut [std::mem::MaybeUninit<u8>]
                            as *mut [u8]);

                        let n = self.channel.read(b)?;
                        buf_window.assume_init(n);
                        buf_window.advance(n);

                        buf_window.filled().len()
                    };

                    // Safety: This is guaranteed to be the number of initialized (and read)
                    // bytes due to the invariants provided by `ReadBuf::filled`.
                    unsafe {
                        buf.advance_mut(n);
                    }
                }
            }
        }
        Err(io::Error::new(
            io::ErrorKind::Other,
            "couldn't read entire item",
        ))
    }

    fn do_send(&mut self, req: OutgoingItem) -> Result<(), io::Error> {
        let msg = self.channel.metadata.create_message(req)?;

        let mut buf = BytesMut::new();
        let data = self.serde_codec.as_mut().serialize(&msg)?;

        self.codec.encode(data, &mut buf)?;
        self.channel.write_all(&buf)
    }
}

impl<IncomingItem, OutgoingItem> From<Channel> for FramedBlocking<IncomingItem, OutgoingItem> {
    fn from(c: Channel) -> Self {
        FramedBlocking {
            codec: Default::default(),
            read_buffer: BytesMut::with_capacity(4000),
            channel: c,
            serde_codec: Box::pin(Default::default()),
        }
    }
}

impl<IncomingItem, OutgoingItem> Clone for FramedBlocking<IncomingItem, OutgoingItem> {
    fn clone(&self) -> Self {
        Self {
            codec: self.codec.clone(),
            serde_codec: Box::pin(Default::default()),
            read_buffer: self.read_buffer.clone(),
            channel: self.channel.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct Request<T> {
    /// Trace context, deadline, and other cross-cutting concerns.
    pub context: context::Context,
    /// Uniquely identifies the request across all requests sent over a single channel.
    pub id: u64,
    /// The request body.
    pub message: T,
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub enum ClientMessage<T> {
    Request(Request<T>),
    Cancel {
        #[serde(default)]
        trace_context: trace::Context,
        /// The ID of the request to cancel.
        request_id: u64,
    },
}

impl<T> TransferHandles for ClientMessage<T>
where
    T: TransferHandles,
{
    fn move_handles<Transport: HandlesTransport>(
        &self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        match self {
            ClientMessage::Request(r) => r.move_handles(transport),
            ClientMessage::Cancel {
                trace_context: _,
                request_id: _,
            } => Ok(()),
        }
    }

    fn receive_handles<Transport: HandlesTransport>(
        &mut self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        match self {
            ClientMessage::Request(r) => r.receive_handles(transport),
            ClientMessage::Cancel {
                trace_context: _,
                request_id: _,
            } => todo!(),
        }
    }
}

impl<T> TransferHandles for Request<T>
where
    T: TransferHandles,
{
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

impl<IncomingItem, OutgoingItem> BlockingTransport<IncomingItem, OutgoingItem>
where
    OutgoingItem: Serialize + TransferHandles,
    IncomingItem: for<'de> Deserialize<'de> + TransferHandles,
{
    fn new_client_message(
        &self,
        item: OutgoingItem,
        deadline: Option<SystemTime>,
    ) -> (u64, ClientMessage<OutgoingItem>) {
        let mut context = context::current();

        if let Some(deadline) = deadline {
            context.deadline = deadline;
        }

        let request_id = self
            .requests_id
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);

        (
            request_id,
            ClientMessage::Request(Request {
                context,
                id: request_id,
                message: item,
            }),
        )
    }

    pub fn set_nonblocking(&mut self, nonblocking: bool) -> io::Result<()> {
        self.transport.channel.set_nonblocking(nonblocking)
    }

    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.transport.channel.set_read_timeout(timeout)
    }

    pub fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.transport.channel.set_write_timeout(timeout)
    }

    pub fn send_ignore_response(&mut self, item: OutgoingItem) -> io::Result<()> {
        let (_, req) = self.new_client_message(item, None);
        self.transport.do_send(req)
    }

    pub fn send(&mut self, item: OutgoingItem) -> io::Result<IncomingItem> {
        let (request_id, req) = self.new_client_message(item, None);
        self.transport.do_send(req)?;

        for resp in self {
            let resp = resp?;
            if resp.request_id == request_id {
                return resp.message.map_err(|e| io::Error::new(e.kind, e.detail));
            }
        }
        Err(io::Error::new(
            io::ErrorKind::Other,
            "Request is without a response",
        ))
    }
}

impl<IncomingItem, OutgoingItem> Iterator for BlockingTransport<IncomingItem, OutgoingItem>
where
    IncomingItem: for<'de> Deserialize<'de> + TransferHandles,
    OutgoingItem: Serialize + TransferHandles,
{
    type Item = io::Result<Response<IncomingItem>>;

    fn next(&mut self) -> Option<io::Result<Response<IncomingItem>>> {
        Some(self.transport.read_item())
    }
}
