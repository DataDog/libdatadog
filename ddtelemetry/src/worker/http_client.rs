// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon::HttpRequestBuilder;
use http::{Request, Response};
use hyper::Body;
use std::{
    fs::File,
    future::Future,
    io::Write,
    pin::Pin,
    sync::{Arc, Mutex},
};

use crate::config::Config;

pub mod header {
    #![allow(clippy::declare_interior_mutable_const)]
    use http::header::HeaderName;
    pub const REQUEST_TYPE: HeaderName = HeaderName::from_static("dd-telemetry-request-type");
    pub const API_VERSION: HeaderName = HeaderName::from_static("dd-telemetry-api-version");
    pub const LIBRARY_LANGUAGE: HeaderName = HeaderName::from_static("dd-client-library-language");
    pub const LIBRARY_VERSION: HeaderName = HeaderName::from_static("dd-client-library-version");
}

pub type ResponseFuture =
    Pin<Box<dyn Future<Output = Result<Response<Body>, hyper::Error>> + Send>>;

pub trait HttpClient {
    fn request(&self, req: Request<hyper::Body>) -> ResponseFuture;
}

pub fn request_builder(c: &Config) -> anyhow::Result<HttpRequestBuilder> {
    match &c.endpoint {
        Some(e) => e.into_request_builder(concat!("telemetry/", env!("CARGO_PKG_VERSION"))),
        None => Err(anyhow::Error::msg(
            "no valid endpoint found, can't build the request".to_string(),
        )),
    }
}

pub fn from_config(c: &Config) -> Box<dyn HttpClient + Sync + Send> {
    match &c.endpoint {
        Some(e) if e.url.scheme_str() == Some("file") => {
            let file_path = ddcommon::decode_uri_path_in_authority(&e.url)
                .expect("file urls should always have been encoded in authority");
            return Box::new(MockClient {
                file: Arc::new(Mutex::new(Box::new(
                    File::create(file_path).expect("Couldn't open mock client file"),
                ))),
            });
        }
        Some(_) | None => {}
    };
    Box::new(HyperClient {
        inner: hyper::Client::builder()
            .pool_idle_timeout(std::time::Duration::from_secs(30))
            .build(ddcommon::connector::Connector::default()),
    })
}

pub struct HyperClient {
    inner: ddcommon::HttpClient,
}

impl HttpClient for HyperClient {
    fn request(&self, req: Request<hyper::Body>) -> ResponseFuture {
        Box::pin(self.inner.request(req))
    }
}

#[derive(Clone)]
pub struct MockClient {
    file: Arc<Mutex<Box<dyn Write + Sync + Send>>>,
}

impl HttpClient for MockClient {
    fn request(&self, mut req: Request<hyper::Body>) -> ResponseFuture {
        let s = self.clone();
        Box::pin(async move {
            let body = hyper::body::to_bytes(req.body_mut()).await?;

            {
                let mut writer = s.file.lock().expect("mutex poisoned");
                writer.write_all(body.as_ref()).unwrap();
                writer.write_all(b"\n").unwrap();
            }

            Ok(Response::builder()
                .status(202)
                .body(hyper::Body::empty())
                .unwrap())
        })
    }
}

#[cfg(test)]
mod tests {
    use ddcommon::HttpRequestBuilder;

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
                .body(hyper::Body::from("hello world\n"))
                .unwrap(),
        )
        .await
        .unwrap();
    }
}
