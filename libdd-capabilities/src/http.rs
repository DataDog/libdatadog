// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! HTTP capability trait and error types.
//!
//! Request and response types are provided by the [`http`] crate, which is a
//! pure-types crate with no platform dependencies (compiles on wasm). The body
//! type is [`bytes::Bytes`].

use crate::maybe_send::{MaybeSend, MaybeSendFuture};
use core::future::Future;
use core::pin::Pin;
use futures_util::StreamExt;

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("Network error: {0}")]
    Network(anyhow::Error),
    #[error("Request timed out")]
    Timeout,
    #[error("Response body error: {0}")]
    ResponseBody(anyhow::Error),
    #[error("Invalid request: {0}")]
    InvalidRequest(anyhow::Error),
    #[error("HTTP error: {0}")]
    Other(anyhow::Error),
}

pub type ChunkFuture<'a> = Pin<Box<dyn MaybeSendFuture<Result<(), HttpError>> + 'a>>;

/// A handle for feeding a [`HttpClientCapability::request_streamed`] request body
/// incrementally, one chunk at a time.
pub trait StreamingBodySender: MaybeSend {
    fn send_chunk(&mut self, data: bytes::Bytes) -> ChunkFuture<'_>;
}

/// Fallback [`StreamingBodySender`] that buffers every chunk in memory and only issues
/// the request once the sender side is dropped.
pub struct BufferingBodySender(futures_channel::mpsc::UnboundedSender<bytes::Bytes>);

impl StreamingBodySender for BufferingBodySender {
    fn send_chunk(&mut self, data: bytes::Bytes) -> ChunkFuture<'_> {
        let result = self
            .0
            .unbounded_send(data)
            .map_err(|e| HttpError::Network(e.into()));
        Box::pin(async move { result })
    }
}

pub type ResponseFuture =
    Pin<Box<dyn MaybeSendFuture<Result<http::Response<bytes::Bytes>, HttpError>>>>;

pub type BodySender = Box<dyn StreamingBodySender>;

pub trait HttpClientCapability: Clone + std::fmt::Debug {
    fn new_client() -> Self;

    fn request(
        &self,
        req: http::Request<bytes::Bytes>,
    ) -> impl Future<Output = Result<http::Response<bytes::Bytes>, HttpError>> + MaybeSend;

    /// Like [`Self::request`], but the request body is provided incrementally, one chunk at a
    /// time, via the returned [`BodySender`].
    fn request_streamed(&self, req: http::Request<()>) -> (BodySender, ResponseFuture)
    where
        Self: MaybeSend + 'static,
    {
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<bytes::Bytes>();
        let this = self.clone();
        let fut = async move {
            let mut body = Vec::new();
            while let Some(chunk) = rx.next().await {
                body.extend_from_slice(&chunk);
            }
            this.request(req.map(|()| bytes::Bytes::from(body))).await
        };
        (Box::new(BufferingBodySender(tx)), Box::pin(fut))
    }
}
