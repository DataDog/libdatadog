// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! HTTP capability implementation using hyper.

use http_body_util::BodyExt;
use libdd_capabilities::http::{HttpClientTrait, HttpError, HttpRequest, HttpResponse, Method};
use libdd_capabilities::maybe_send::MaybeSend;
use libdd_common::{connector::Connector, hyper_migration};

pub struct DefaultHttpClient {
    client: hyper_migration::GenericHttpClient<Connector>,
}

impl HttpClientTrait for DefaultHttpClient {
    fn new_client() -> Self {
        Self {
            client: hyper_migration::new_default_client(),
        }
    }

    #[allow(clippy::manual_async_fn)]
    fn request(
        &self,
        req: HttpRequest,
    ) -> impl std::future::Future<Output = Result<HttpResponse, HttpError>> + MaybeSend {
        let client = self.client.clone();
        async move {
            let uri: hyper::Uri = req
                .url()
                .parse()
                .map_err(|e| HttpError::InvalidRequest(format!("Invalid URL: {}", e)))?;

            let method = match req.method() {
                Method::Get => hyper::Method::GET,
                Method::Head => hyper::Method::HEAD,
                Method::Delete => hyper::Method::DELETE,
                Method::Options => hyper::Method::OPTIONS,
                Method::Post => hyper::Method::POST,
                Method::Put => hyper::Method::PUT,
                Method::Patch => hyper::Method::PATCH,
            };

            let mut builder = hyper::Request::builder().method(method).uri(uri);

            for (key, value) in req.headers() {
                builder = builder.header(key.as_str(), value.as_str());
            }

            let method_str = req.method_str();
            let accepts_body = req.method().accepts_body();
            let body = req.into_body();
            if body.is_some() && !accepts_body {
                return Err(HttpError::InvalidRequest(format!(
                    "method {} does not accept a request body",
                    method_str
                )));
            }

            let body = hyper_migration::Body::from(body.unwrap_or_default());
            let hyper_req = builder.body(body).map_err(|e| {
                HttpError::InvalidRequest(format!("Failed to build request: {}", e))
            })?;

            let response = client
                .request(hyper_req)
                .await
                .map_err(|e| HttpError::Network(format!("Request failed: {}", e)))?;

            let status = response.status().as_u16();
            let headers: Vec<(String, String)> = response
                .headers()
                .iter()
                .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
                .collect();

            let body_collected = response.into_body().collect().await.map_err(|e| {
                HttpError::ResponseBody(format!("Failed to read response body: {}", e))
            })?;
            let body_bytes = body_collected.to_bytes();

            Ok(HttpResponse {
                status,
                headers,
                body: body_bytes.to_vec(),
            })
        }
    }
}
