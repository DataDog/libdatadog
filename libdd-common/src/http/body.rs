// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Request/response body type for the shared HTTP foundation.
//!
//! [`Body`] is a hyper-compatible body enum that consumers and backends use to
//! ferry bytes through the client. Native (non-wasm) only.

#![cfg(not(target_arch = "wasm32"))]

use std::task::Poll;

use http_body_util::BodyExt;
use hyper::body::Incoming;
use pin_project::pin_project;

use super::error::{ClientError, Error};

/// Hyper-compatible HTTP body used by the shared foundation. Covers the
/// common shapes consumers need: empty, in-memory bytes, a boxed dynamic
/// body, an `mpsc`-backed streaming body, and an inbound `hyper::Body`.
#[pin_project(project=BodyProj)]
#[derive(Debug)]
pub enum Body {
    /// In-memory body backed by a single `Bytes` buffer.
    Single(#[pin] http_body_util::Full<hyper::body::Bytes>),
    /// Empty body (no frames).
    Empty(#[pin] http_body_util::Empty<hyper::body::Bytes>),
    /// Type-erased body for cases where the concrete shape is not known.
    Boxed(#[pin] http_body_util::combinators::BoxBody<hyper::body::Bytes, anyhow::Error>),
    /// Streaming body fed by a [`Sender`] (see [`Body::channel`]).
    Channel(#[pin] tokio::sync::mpsc::Receiver<hyper::body::Bytes>),
    /// Inbound body from a hyper response.
    Incoming(#[pin] hyper::body::Incoming),
}

/// Sender half of a channel-backed [`Body`]. Construct via [`Body::channel`].
pub struct Sender {
    tx: tokio::sync::mpsc::Sender<hyper::body::Bytes>,
}

impl Sender {
    /// Send a chunk of bytes into the body channel.
    pub async fn send_data(&self, data: hyper::body::Bytes) -> anyhow::Result<()> {
        self.tx.send(data).await?;
        Ok(())
    }
}

impl Body {
    /// Construct an empty body.
    pub fn empty() -> Self {
        Body::Empty(http_body_util::Empty::new())
    }

    /// Construct a body from a single `Bytes` buffer.
    pub fn from_bytes(bytes: hyper::body::Bytes) -> Self {
        Body::Single(http_body_util::Full::new(bytes))
    }

    /// Box a body that is `Send + Sync + 'static`. Errors are coerced through
    /// `anyhow::Error`.
    pub fn boxed<
        E: std::error::Error + Sync + Send + 'static,
        T: hyper::body::Body<Data = hyper::body::Bytes, Error = E> + Sync + Send + 'static,
    >(
        body: T,
    ) -> Self {
        Body::Boxed(body.map_err(anyhow::Error::from).boxed())
    }

    /// Construct a streaming body and a [`Sender`] to feed it.
    pub fn channel() -> (Sender, Self) {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        (Sender { tx }, Body::Channel(rx))
    }

    /// Wrap an inbound hyper body.
    pub fn incoming(incoming: Incoming) -> Self {
        Body::Incoming(incoming)
    }
}

impl Default for Body {
    fn default() -> Self {
        Body::empty()
    }
}

impl From<&'static str> for Body {
    fn from(s: &'static str) -> Self {
        Body::from_bytes(hyper::body::Bytes::from_static(s.as_bytes()))
    }
}

impl From<Vec<u8>> for Body {
    fn from(s: Vec<u8>) -> Self {
        Body::from_bytes(hyper::body::Bytes::from(s))
    }
}

impl From<String> for Body {
    fn from(s: String) -> Self {
        Body::from_bytes(hyper::body::Bytes::from(s))
    }
}

impl hyper::body::Body for Body {
    type Data = hyper::body::Bytes;

    type Error = Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        match self.project() {
            BodyProj::Single(pin) => pin.poll_frame(cx).map_err(Error::Infallible),
            BodyProj::Empty(pin) => pin.poll_frame(cx).map_err(Error::Infallible),
            BodyProj::Boxed(pin) => pin.poll_frame(cx).map_err(Error::Other),
            BodyProj::Channel(pin) => {
                let data = match pin.get_mut().poll_recv(cx) {
                    Poll::Ready(Some(data)) => data,
                    Poll::Ready(None) => return Poll::Ready(None),
                    Poll::Pending => return Poll::Pending,
                };
                Poll::Ready(Some(Ok(hyper::body::Frame::data(data))))
            }
            BodyProj::Incoming(pin) => pin
                .poll_frame(cx)
                .map_err(|e| Error::Client(ClientError::from(e))),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Body::Single(body) => body.is_end_stream(),
            Body::Empty(body) => body.is_end_stream(),
            Body::Boxed(body) => body.is_end_stream(),
            Body::Channel(body) => body.is_closed() && body.is_empty(),
            Body::Incoming(body) => body.is_end_stream(),
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match self {
            Body::Single(body) => body.size_hint(),
            Body::Empty(body) => body.size_hint(),
            Body::Boxed(body) => body.size_hint(),
            Body::Channel(_) => http_body::SizeHint::default(),
            Body::Incoming(body) => body.size_hint(),
        }
    }
}
