// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use bytes::Bytes;
use libdd_capabilities::http::HttpError;
use libdd_capabilities::HttpClientTrait;
use libdd_capabilities_impl::DefaultHttpClient;
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

pub type ResponseFuture =
    Pin<Box<dyn Future<Output = Result<http::Response<Bytes>, HttpError>> + Send>>;

pub trait HttpClient {
    fn request(&self, req: http::Request<Bytes>) -> ResponseFuture;
}

pub fn request_builder(c: &Config) -> anyhow::Result<http::request::Builder> {
    match &c.endpoint {
        Some(e) => {
            debug!(
                endpoint.url = %e.url,
                endpoint.timeout_ms = e.timeout_ms,
                telemetry.version = env!("CARGO_PKG_VERSION"),
                "Building telemetry request"
            );
            let mut builder = http::Request::builder().uri(e.url.clone());
            builder =
                e.set_standard_headers(builder, concat!("telemetry/", env!("CARGO_PKG_VERSION")));
            if c.debug_enabled {
                debug!(
                    telemetry.debug_enabled = true,
                    "Telemetry debug mode enabled"
                );
                builder = builder.header(header::DEBUG_ENABLED, "true");
            }
            Ok(builder)
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
    Box::new(CapabilitiesClient)
}

pub struct CapabilitiesClient;

impl HttpClient for CapabilitiesClient {
    fn request(&self, req: http::Request<Bytes>) -> ResponseFuture {
        Box::pin(async move {
            let client = DefaultHttpClient::new_client();
            client.request(req).await
        })
    }
}

#[derive(Clone)]
pub struct MockClient {
    file: Arc<Mutex<Box<dyn Write + Sync + Send>>>,
}

impl HttpClient for MockClient {
    fn request(&self, req: http::Request<Bytes>) -> ResponseFuture {
        let s = self.clone();
        Box::pin(async move {
            debug!("MockClient writing request to file");
            let mut body = req.into_body().to_vec();
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
                        return Err(HttpError::Other(format!("Mock file write error: {e}")));
                    }
                }
            }

            debug!(http.status = 202, "MockClient returning success response");
            http::Response::builder()
                .status(202)
                .body(Bytes::new())
                .map_err(|e| HttpError::Other(format!("Failed to build response: {e}")))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_mock_client() {
        let output: Vec<u8> = Vec::new();
        let c = MockClient {
            file: Arc::new(Mutex::new(Box::new(output))),
        };
        c.request(
            http::Request::builder()
                .body(Bytes::from("hello world\n"))
                .unwrap(),
        )
        .await
        .unwrap();
    }
}
