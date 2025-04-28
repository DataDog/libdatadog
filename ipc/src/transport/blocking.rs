// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    io::{self, Read, Write},
    mem::MaybeUninit,
    pin::Pin,
    sync::{atomic::AtomicU64, Arc},
    time::Duration,
};

use bytes::{BufMut, BytesMut};
use serde::{Deserialize, Serialize};
use tarpc::{context::Context, ClientMessage, Request, Response};

use tokio_serde::{Deserializer, Serializer};

use tokio_util::codec::{Decoder, Encoder, LengthDelimitedCodec};

use crate::{
    handles::TransferHandles,
    platform::{Channel, Message},
};

use super::DefaultCodec;

pub struct BlockingTransport<IncomingItem, OutgoingItem> {
    requests_id: Arc<AtomicU64>,
    transport: FramedBlocking<Response<IncomingItem>, ClientMessage<OutgoingItem>>,
}

impl<IncomingItem, OutgoingItem> From<Channel> for BlockingTransport<IncomingItem, OutgoingItem> {
    fn from(c: Channel) -> Self {
        BlockingTransport {
            requests_id: Arc::from(AtomicU64::new(0)),
            transport: c.into(),
        }
    }
}

#[cfg(unix)]
impl<IncomingItem, OutgoingItem> From<std::os::unix::net::UnixStream>
    for BlockingTransport<IncomingItem, OutgoingItem>
{
    fn from(s: std::os::unix::net::UnixStream) -> Self {
        BlockingTransport {
            requests_id: Arc::from(AtomicU64::new(0)),
            transport: Channel::from(s).into(),
        }
    }
}

pub struct FramedBlocking<IncomingItem, OutgoingItem> {
    codec: LengthDelimitedCodec,
    read_buffer: BytesMut,
    channel: Channel,
    serde_codec: Pin<Box<DefaultCodec<Message<IncomingItem>, Message<OutgoingItem>>>>,
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
                        // it is performing an UB operation by using uninitiallized memory -
                        // although in practice its somewhat defined
                        // there are still some unknowns WRT to future behaviors

                        // TODO: make sure this optimization is really needed - once BenchPlatform
                        // is connected to libdatadog benchmark unfilled_mut
                        // vs initialize_unfilled - and if the difference is negligible - then lets
                        // switch to implementation that doesn't use UB.
                        let b = &mut *(buf_window.unfilled_mut() as *mut [MaybeUninit<u8>]
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
        Err(io::Error::other("couldn't read entire item"))
    }

    fn do_send(&mut self, req: OutgoingItem) -> Result<(), io::Error> {
        let msg = self.channel.create_message(req)?;

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

impl<IncomingItem, OutgoingItem> BlockingTransport<IncomingItem, OutgoingItem>
where
    OutgoingItem: Serialize + TransferHandles,
    IncomingItem: for<'de> Deserialize<'de> + TransferHandles,
{
    fn new_client_message(
        &self,
        item: OutgoingItem,
        context: Context,
    ) -> (u64, ClientMessage<OutgoingItem>) {
        let request_id = self
            .requests_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

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

    pub fn is_closed(&self) -> bool {
        // The blocking transport is not supposed to be readable on the client side unless it's a
        // response. So, outside of waiting for a response, it will never be readable unless
        // the server side closed its socket.
        self.transport.channel.probe_readable()
    }

    pub fn send(&mut self, item: OutgoingItem) -> io::Result<()> {
        let mut ctx = Context::current();
        ctx.discard_response = true;
        let (_, req) = self.new_client_message(item, ctx);
        self.transport.do_send(req)
    }

    pub fn call(&mut self, item: OutgoingItem) -> io::Result<IncomingItem> {
        let (request_id, req) = self.new_client_message(item, Context::current());
        self.transport.do_send(req)?;

        for resp in self {
            let resp = resp?;
            if resp.request_id == request_id {
                return resp.message.map_err(|e| io::Error::new(e.kind, e.detail));
            }
        }
        Err(io::Error::other("Request is without a response"))
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
