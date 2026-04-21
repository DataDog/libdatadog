// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Native HTTP client implementation backed by hyper.

mod native {
    use libdd_capabilities::http::{HttpClientTrait, HttpError};
    use libdd_capabilities::maybe_send::MaybeSend;
    use libdd_common::connector::Connector;
    use libdd_common::http_common::{new_default_client, Body, GenericHttpClient};

    use http_body_util::BodyExt;

    #[derive(Clone)]
    pub struct DefaultHttpClient {
        client: GenericHttpClient<Connector>,
    }

    impl std::fmt::Debug for DefaultHttpClient {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("DefaultHttpClient").finish()
        }
    }

    impl HttpClientTrait for DefaultHttpClient {
        fn new_client() -> Self {
            Self {
                client: new_default_client(),
            }
        }

        #[allow(clippy::manual_async_fn)]
        fn request(
            &self,
            req: http::Request<bytes::Bytes>,
        ) -> impl std::future::Future<Output = Result<http::Response<bytes::Bytes>, HttpError>> + MaybeSend
        {
            let client = self.client.clone();
            async move {
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
    }
}

pub use native::DefaultHttpClient;
