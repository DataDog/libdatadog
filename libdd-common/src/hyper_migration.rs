// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::fmt;
use std::{convert::Infallible, task::Poll};

use crate::connector::Connector;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use pin_project::pin_project;
// Need aliases because cbindgen is not smart enough to figure type aliases
use http::Request as HyperRequest;

/// Create a new default configuration hyper client for fixed interval sending.
///
/// This client does not keep connections because otherwise we would get a pipe closed
/// every second connection because of low keep alive in the agent.
///
/// This is on general not a problem if we use the client once every tens of seconds.
pub fn new_client_periodic() -> GenericHttpClient<Connector> {
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::default())
        .pool_max_idle_per_host(0)
        .build(Connector::default())
}

/// Create a new default configuration hyper client.
///
/// It will keep connections open for a longer time and reuse them.
pub fn new_default_client() -> GenericHttpClient<Connector> {
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::default())
        .build(Connector::default())
}

pub type HttpResponse = hyper::Response<Body>;
pub type HttpRequest = HyperRequest<Body>;
pub type ClientError = hyper_util::client::legacy::Error;
pub type ResponseFuture = hyper_util::client::legacy::ResponseFuture;

pub fn into_response(response: hyper::Response<Incoming>) -> HttpResponse {
    response.map(Body::Incoming)
}

#[derive(Debug)]
pub enum Error {
    Hyper(hyper::Error),
    Legacy(hyper_util::client::legacy::Error),
    Other(anyhow::Error),
    Infallible(Infallible),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Hyper(e) => write!(f, "hyper error: {e}"),
            Error::Legacy(e) => write!(f, "hyper legacy error: {e}"),
            Error::Infallible(e) => match *e {},
            Error::Other(e) => write!(f, "other error: {e}"),
        }
    }
}

impl From<hyper_util::client::legacy::Error> for Error {
    fn from(value: hyper_util::client::legacy::Error) -> Self {
        Self::Legacy(value)
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Other(value.into())
    }
}

impl From<http::Error> for Error {
    fn from(value: http::Error) -> Self {
        Self::Other(value.into())
    }
}

impl std::error::Error for Error {}

pub fn mock_response(
    builder: http::response::Builder,
    body: hyper::body::Bytes,
) -> anyhow::Result<HttpResponse> {
    Ok(builder.body(Body::from_bytes(body))?)
}

pub fn empty_response(builder: http::response::Builder) -> Result<HttpResponse, Error> {
    Ok(builder.body(Body::empty())?)
}

#[pin_project(project=BodyProj)]
#[derive(Debug)]
pub enum Body {
    Single(#[pin] http_body_util::Full<hyper::body::Bytes>),
    Empty(#[pin] http_body_util::Empty<hyper::body::Bytes>),
    Boxed(#[pin] http_body_util::combinators::BoxBody<hyper::body::Bytes, anyhow::Error>),
    Channel(#[pin] tokio::sync::mpsc::Receiver<hyper::body::Bytes>),
    Incoming(#[pin] hyper::body::Incoming),
}

pub struct Sender {
    tx: tokio::sync::mpsc::Sender<hyper::body::Bytes>,
}

impl Sender {
    pub async fn send_data(&self, data: hyper::body::Bytes) -> anyhow::Result<()> {
        self.tx.send(data).await?;
        Ok(())
    }
}

impl Body {
    pub fn empty() -> Self {
        Body::Empty(http_body_util::Empty::new())
    }

    pub fn from_bytes(bytes: hyper::body::Bytes) -> Self {
        Body::Single(http_body_util::Full::new(bytes))
    }

    pub fn boxed<
        E: std::error::Error + Sync + Send + 'static,
        T: hyper::body::Body<Data = hyper::body::Bytes, Error = E> + Sync + Send + 'static,
    >(
        body: T,
    ) -> Self {
        Body::Boxed(body.map_err(anyhow::Error::from).boxed())
    }

    pub fn channel() -> (Sender, Self) {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        (Sender { tx }, Body::Channel(rx))
    }

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
            BodyProj::Incoming(pin) => pin.poll_frame(cx).map_err(Error::Hyper),
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

pub type GenericHttpClient<C> = hyper_util::client::legacy::Client<C, Body>;

pub fn client_builder() -> hyper_util::client::legacy::Builder {
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::default())
}
