// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use http_body_util::BodyExt;
use libdd_common::{hyper_migration, HttpRequestBuilder};
use std::{
    fs::OpenOptions,
    future::Future,
    io::Write,
    pin::Pin,
    sync::{Arc, Mutex},
};

use crate::config::Config;
use tracing::{debug, error};

pub mod header {
    #![allow(clippy::declare_interior_mutable_const)]
    use http::header::HeaderName;
    pub const REQUEST_TYPE: HeaderName = HeaderName::from_static("dd-telemetry-request-type");
    pub const API_VERSION: HeaderName = HeaderName::from_static("dd-telemetry-api-version");
    pub const LIBRARY_LANGUAGE: HeaderName = HeaderName::from_static("dd-client-library-language");
    pub const LIBRARY_VERSION: HeaderName = HeaderName::from_static("dd-client-library-version");

    /// Header key for whether to enable debug mode of telemetry.
    pub const DEBUG_ENABLED: HeaderName = HeaderName::from_static("dd-telemetry-debug-enabled");
}

pub type ResponseFuture = Pin<
    Box<dyn Future<Output = Result<hyper_migration::HttpResponse, hyper_migration::Error>> + Send>,
>;

pub trait HttpClient {
    fn request(&self, req: hyper_migration::HttpRequest) -> ResponseFuture;
}

pub fn request_builder(c: &Config) -> anyhow::Result<HttpRequestBuilder> {
    match &c.endpoint {
        Some(e) => {
            debug!(
                endpoint.url = %e.url,
                endpoint.timeout_ms = e.timeout_ms,
                telemetry.version = env!("CARGO_PKG_VERSION"),
                "Building telemetry request"
            );
            let mut builder =
                e.to_request_builder(concat!("telemetry/", env!("CARGO_PKG_VERSION")));
            if c.debug_enabled {
                debug!(
                    telemetry.debug_enabled = true,
                    "Telemetry debug mode enabled"
                );
                builder = Ok(builder?.header(header::DEBUG_ENABLED, "true"))
            }
            builder
        }
        None => {
            error!("No valid telemetry endpoint found, cannot build request");
            Err(anyhow::Error::msg(
                "no valid endpoint found, can't build the request".to_string(),
            ))
        }
    }
}

pub fn from_config(c: &Config) -> Box<dyn HttpClient + Sync + Send> {
    match &c.endpoint {
        Some(e) if e.url.scheme_str() == Some("file") => {
            #[allow(clippy::expect_used)]
            let file_path = libdd_common::decode_uri_path_in_authority(&e.url)
                .expect("file urls should always have been encoded in authority");
            debug!(
                file.path = ?file_path,
                "Using file-based mock telemetry client"
            );
            return Box::new(MockClient {
                #[allow(clippy::expect_used)]
                file: Arc::new(Mutex::new(Box::new(
                    OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(file_path.as_path())
                        .expect("Couldn't open mock client file"),
                ))),
            });
        }
        Some(e) => {
            debug!(
                endpoint.url = %e.url,
                endpoint.timeout_ms = e.timeout_ms,
                "Using HTTP telemetry client"
            );
        }
        None => {
            debug!(
                endpoint = "default",
                "No telemetry endpoint configured, using default HTTP client"
            );
        }
    };
    Box::new(HyperClient {
        inner: hyper_migration::new_client_periodic(),
    })
}

pub struct HyperClient {
    inner: libdd_common::HttpClient,
}

impl HttpClient for HyperClient {
    fn request(&self, req: hyper_migration::HttpRequest) -> ResponseFuture {
        let resp = self.inner.request(req);
        Box::pin(async move {
            match resp.await {
                Ok(response) => Ok(hyper_migration::into_response(response)),
                Err(e) => Err(e.into()),
            }
        })
    }
}

#[derive(Clone)]
pub struct MockClient {
    file: Arc<Mutex<Box<dyn Write + Sync + Send>>>,
}

impl HttpClient for MockClient {
    fn request(&self, req: hyper_migration::HttpRequest) -> ResponseFuture {
        let s = self.clone();
        Box::pin(async move {
            debug!("MockClient writing request to file");
            let mut body = req.collect().await?.to_bytes().to_vec();
            body.push(b'\n');

            {
                #[allow(clippy::expect_used)]
                let mut writer = s.file.lock().expect("mutex poisoned");

                match writer.write_all(body.as_ref()) {
                    Ok(()) => debug!(
                        file.bytes_written = body.len(),
                        "Successfully wrote payload to mock file"
                    ),
                    Err(e) => {
                        error!(
                            error = %e,
                            "Failed to write to mock file"
                        );
                        return Err(hyper_migration::Error::from(e));
                    }
                }
            }

            debug!(http.status = 202, "MockClient returning success response");
            hyper_migration::empty_response(hyper::Response::builder().status(202))
        })
    }
}

#[cfg(test)]
mod tests {
    use libdd_common::HttpRequestBuilder;

    use super::*;

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_mock_client() {
        let output: Vec<u8> = Vec::new();
        let c = MockClient {
            file: Arc::new(Mutex::new(Box::new(output))),
        };
        c.request(
            HttpRequestBuilder::new()
                .body(hyper_migration::Body::from("hello world\n"))
                .unwrap(),
        )
        .await
        .unwrap();
    }
}
