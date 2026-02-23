// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! HTTP capability implementation using hyper.

use bytes::Bytes;
use http_body_util::BodyExt;
use libdd_capabilities::http::{HttpClientTrait, HttpError};
use libdd_capabilities::maybe_send::MaybeSend;
use libdd_common::{connector::Connector, http_common};

pub struct DefaultHttpClient {
    client: http_common::GenericHttpClient<Connector>,
}

impl HttpClientTrait for DefaultHttpClient {
    fn new_client() -> Self {
        Self {
            client: http_common::new_default_client(),
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn request(
        &self,
        req: http::Request<Bytes>,
    ) -> impl std::future::Future<Output = Result<http::Response<Bytes>, HttpError>> + MaybeSend
    {
        let client = self.client.clone();
        async move {
            let (parts, body) = req.into_parts();
            let hyper_req = hyper::Request::from_parts(parts, http_common::Body::from_bytes(body));

            let response = client
                .request(hyper_req)
                .await
                .map_err(|e| HttpError::Network(format!("Request failed: {}", e)))?;

            let (parts, body) = response.into_parts();
            let collected = body
                .collect()
                .await
                .map_err(|e| {
                    HttpError::ResponseBody(format!("Failed to read response body: {}", e))
                })?
                .to_bytes();

            Ok(http::Response::from_parts(parts, collected))
        }
    }
}
