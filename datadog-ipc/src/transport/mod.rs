// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod blocking;

use std::{
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

// TODO keep Json for now, however MessagePack seems to fail at deserialization

use pin_project::pin_project;
use tokio_serde::formats::Bincode;

use tokio_serde::Framed as SerdeFramed;

use futures::{Sink, Stream};
use serde::{Deserialize, Serialize};

use tokio_util::codec::{Framed, LengthDelimitedCodec};

use super::{
    handles::TransferHandles,
    platform::{metadata::ChannelMetadata, AsyncChannel, Channel, Message},
};

pub type DefaultCodec<Item, SinkItem> = Bincode<Item, SinkItem>;

type DefaultSerdeFramed<Item, SinkItem> = SerdeFramed<
    Framed<AsyncChannel, LengthDelimitedCodec>,
    Message<Item>,
    Message<SinkItem>,
    DefaultCodec<Message<Item>, Message<SinkItem>>,
>;

/// A transport that serializes to, and deserializes from, a byte stream.
#[pin_project]
pub struct Transport<Item, SinkItem> {
    #[pin]
    inner: DefaultSerdeFramed<Item, SinkItem>,

    channel_metadata: Arc<Mutex<ChannelMetadata>>,
}

impl<Item, SinkItem> Transport<Item, SinkItem> {
    /// Returns the inner transport over which messages are sent and received.
    pub fn get_ref(&self) -> &AsyncChannel {
        self.inner.get_ref().get_ref()
    }
}

impl<CodecError, Item, SinkItem> Stream for Transport<Item, SinkItem>
where
    Item: for<'a> Deserialize<'a> + TransferHandles,
    CodecError: Into<Box<dyn std::error::Error + Send + Sync>>,
    DefaultSerdeFramed<Item, SinkItem>: Stream<Item = Result<Message<Item>, CodecError>>,
{
    type Item = io::Result<Item>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<io::Result<Item>>> {
        let this = self.project();
        this.inner
            .poll_next(cx)
            .map(|res| match res {
                Some(Ok(message)) => Some(
                    #[allow(clippy::unwrap_used)]
                    this.channel_metadata
                        .lock()
                        .unwrap()
                        .unwrap_message(message)
                        .map_err(Into::into),
                ),
                Some(Err(e)) => Some(Err(e.into())),
                None => None,
            })
            .map_err(io::Error::other)
    }
}

impl<CodecError, Item, SinkItem> Sink<SinkItem> for Transport<Item, SinkItem>
where
    SinkItem: Serialize + TransferHandles,
    CodecError: Into<Box<dyn std::error::Error + Send + Sync>>,
    DefaultSerdeFramed<Item, SinkItem>: Sink<Message<SinkItem>, Error = CodecError>,
{
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.project()
            .inner
            .poll_ready(cx)
            .map_err(io::Error::other)
    }

    fn start_send(self: Pin<&mut Self>, item: SinkItem) -> io::Result<()> {
        let this = self.project();
        #[allow(clippy::unwrap_used)]
        let mut message = this.channel_metadata.lock().unwrap();
        let message = message.create_message(item)?;

        this.inner.start_send(message).map_err(io::Error::other)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.project()
            .inner
            .poll_flush(cx)
            .map_err(io::Error::other)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.project()
            .inner
            .poll_close(cx)
            .map_err(io::Error::other)
    }
}

/// Constructs a new transport from a framed transport and a serialization codec.
fn new<Item, SinkItem>(
    io: AsyncChannel,
    codec: DefaultCodec<Message<Item>, Message<SinkItem>>,
) -> Transport<Item, SinkItem>
where
    Item: for<'de> Deserialize<'de>,
    SinkItem: Serialize,
{
    let channel_metadata = io.metadata.clone();
    let mut length_delimited = LengthDelimitedCodec::new();
    length_delimited.set_max_frame_length(100_000_000);
    Transport {
        inner: SerdeFramed::new(Framed::new(io, length_delimited), codec),
        channel_metadata,
    }
}

pub type SymmetricalTransport<T> = Transport<T, T>;

impl<Item, SinkItem> From<AsyncChannel> for Transport<Item, SinkItem>
where
    Item: for<'de> Deserialize<'de> + TransferHandles,
    SinkItem: Serialize + TransferHandles,
{
    fn from(channel: AsyncChannel) -> Self {
        let codec = DefaultCodec::default();
        new(channel, codec)
    }
}

impl<Item, SinkItem> TryFrom<Channel> for Transport<Item, SinkItem>
where
    Item: for<'de> Deserialize<'de> + TransferHandles,
    SinkItem: Serialize + TransferHandles,
{
    type Error = <AsyncChannel as TryFrom<Channel>>::Error;

    fn try_from(channel: Channel) -> Result<Self, Self::Error> {
        Ok(Self::from(AsyncChannel::try_from(channel)?))
    }
}
