// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::fmt;
use std::{convert::Infallible, error::Error as _, task::Poll};

use crate::connector::Connector;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use pin_project::pin_project;
use thiserror::Error;

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

#[derive(Debug, Clone, Copy)]
pub enum ErrorKind {
    Parse,
    Closed,
    Canceled,
    Incomplete,
    WriteAborted,
    ParseStatus,
    Timeout,
    Other,
}

#[derive(Debug, Error)]
pub struct ClientError {
    source: anyhow::Error,
    kind: ErrorKind,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(f)
    }
}

impl ClientError {
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

impl From<hyper::Error> for ClientError {
    fn from(source: hyper::Error) -> Self {
        use ErrorKind::*;
        let kind = if source.is_canceled() {
            Canceled
        } else if source.is_parse() {
            Parse
        } else if source.is_parse_status() {
            ParseStatus
        } else if source.is_incomplete_message() {
            Incomplete
        } else if source.is_body_write_aborted() {
            WriteAborted
        } else if source.is_timeout() {
            Timeout
        } else if source.is_closed() {
            Closed
        } else {
            Other
        };
        Self {
            kind,
            source: source.into(),
        }
    }
}

pub type HttpResponse = http::Response<Body>;
pub type HttpRequest = http::Request<Body>;
pub type HttpRequestError = hyper_util::client::legacy::Error;

pub type ResponseFuture = hyper_util::client::legacy::ResponseFuture;

pub fn into_response(response: hyper::Response<Incoming>) -> HttpResponse {
    response.map(Body::Incoming)
}

pub fn into_error(err: HttpRequestError) -> ClientError {
    let kind = if let Some(source) = err.source().and_then(|s| s.downcast_ref::<Error>()) {
        match source {
            Error::Client(client_error) => client_error.kind,
            Error::Other(_) => ErrorKind::Other,
            Error::Infallible(infallible) => match *infallible {},
        }
    } else if err.is_connect() {
        ErrorKind::Closed
    } else {
        ErrorKind::Other
    };
    ClientError {
        source: err.into(),
        kind,
    }
}

pub async fn collect_response_bytes(response: HttpResponse) -> Result<bytes::Bytes, Error> {
    Ok(response.into_body().collect().await?.to_bytes())
}

#[derive(Debug)]
pub enum Error {
    Client(ClientError),
    Other(anyhow::Error),
    Infallible(Infallible),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Client(e) => write!(f, "client error: {e}"),
            Error::Infallible(e) => match *e {},
            Error::Other(e) => write!(f, "other error: {e}"),
        }
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

pub type GenericHttpClient<C> = hyper_util::client::legacy::Client<C, Body>;

pub fn client_builder() -> hyper_util::client::legacy::Builder {
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::default())
}
