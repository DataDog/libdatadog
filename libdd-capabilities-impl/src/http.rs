// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Native HTTP client implementation backed by hyper.

mod native {
    use std::fs::OpenOptions;
    use std::future::Future;
    use std::io::Write;
    use std::sync::{Arc, OnceLock};

    use libdd_capabilities::http::{
        BodySender, ChunkFuture, HttpClientCapability, HttpError, ResponseFuture,
        StreamingBodySender,
    };
    use libdd_capabilities::maybe_send::MaybeSend;
    use libdd_common::connector::Connector;
    use libdd_common::http_common::{
        new_client_periodic, new_default_client, Body, GenericHttpClient,
    };

    use http_body_util::BodyExt;

    #[derive(Clone)]
    pub struct NativeHttpClient {
        client: Arc<OnceLock<GenericHttpClient<Connector>>>,
        connection_pooling: bool,
    }

    pub struct NativeBodySender(libdd_common::http_common::Sender);

    impl StreamingBodySender for NativeBodySender {
        fn send_chunk(&mut self, data: bytes::Bytes) -> ChunkFuture<'_> {
            Box::pin(async move { self.0.send_data(data).await.map_err(HttpError::Network) })
        }
    }

    impl std::fmt::Debug for NativeHttpClient {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("NativeHttpClient")
                .field("initialized", &self.client.get().is_some())
                .field("connection_pooling", &self.connection_pooling)
                .finish()
        }
    }

    impl NativeHttpClient {
        /// Like [`HttpClientCapability::new_client`], but disables connection pooling.
        ///
        /// Intended for clients that issue requests on a fixed interval (e.g. remote
        /// config polling): the agent's low keep-alive setting can close an idle
        /// connection between polls, which turns a pooled/reused connection into
        /// intermittent request failures.
        pub fn new_without_connection_pooling() -> Self {
            Self {
                client: Arc::new(OnceLock::new()),
                connection_pooling: false,
            }
        }
    }

    /// Write `body` as a newline-terminated record to the file referenced by `uri` (which must
    /// have a `file://` scheme), then return a synthetic 202 response.
    fn write_to_file_endpoint(
        uri: &http::Uri,
        body: bytes::Bytes,
    ) -> Result<http::Response<bytes::Bytes>, HttpError> {
        let path = libdd_common::decode_uri_path_in_authority(uri)
            .map_err(|e| HttpError::Other(anyhow::anyhow!("invalid file:// URI: {e}")))?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| HttpError::Other(anyhow::anyhow!("opening {path:?}: {e}")))?;
        let mut record = body.to_vec();
        record.push(b'\n');
        file.write_all(&record)
            .map_err(|e| HttpError::Other(anyhow::anyhow!("writing {path:?}: {e}")))?;

        http::Response::builder()
            .status(http::StatusCode::ACCEPTED)
            .body(bytes::Bytes::new())
            .map_err(|e| HttpError::Other(e.into()))
    }

    impl HttpClientCapability for NativeHttpClient {
        fn new_client() -> Self {
            Self {
                client: Arc::new(OnceLock::new()),
                connection_pooling: true,
            }
        }

        #[allow(clippy::manual_async_fn)]
        fn request(
            &self,
            req: http::Request<bytes::Bytes>,
        ) -> impl Future<Output = Result<http::Response<bytes::Bytes>, HttpError>> + MaybeSend
        {
            let connection_pooling = self.connection_pooling;
            let client_lock = self.client.clone();
            async move {
                // file:// URIs short-circuit to the on-disk recorder used by tests.
                if req.uri().scheme_str() == Some("file") {
                    let (parts, body) = req.into_parts();
                    return write_to_file_endpoint(&parts.uri, body);
                }

                let client = client_lock
                    .get_or_init(|| {
                        if connection_pooling {
                            new_default_client()
                        } else {
                            new_client_periodic()
                        }
                    })
                    .clone();
                let hyper_req = req.map(Body::from_bytes);

                let response = client
                    .request(hyper_req)
                    .await
                    .map_err(|e| HttpError::Network(e.into()))?;

                let (parts, body) = response.into_parts();
                let collected = body
                    .collect()
                    .await
                    .map_err(|e| HttpError::ResponseBody(e.into()))?
                    .to_bytes();

                Ok(http::Response::from_parts(parts, collected))
            }
        }

        fn request_streamed(&self, req: http::Request<()>) -> (BodySender, ResponseFuture) {
            let client = self.client.get_or_init(new_default_client).clone();
            let (sender, body) = Body::channel();
            let hyper_req = req.map(|()| body);
            let fut = async move {
                let response = client
                    .request(hyper_req)
                    .await
                    .map_err(|e| HttpError::Network(e.into()))?;

                let (parts, body) = response.into_parts();
                let collected = body
                    .collect()
                    .await
                    .map_err(|e| HttpError::ResponseBody(e.into()))?
                    .to_bytes();

                Ok(http::Response::from_parts(parts, collected))
            };
            (Box::new(NativeBodySender(sender)), Box::pin(fut))
        }
    }
}

pub use native::NativeHttpClient;
