// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::Endpoint;
use http::{Request, Response};
use hyper::Body;
use std::{
    fs::File,
    future::Future,
    io::Write,
    pin::Pin,
    sync::{Arc, Mutex},
};

pub type ResponseFuture =
    Pin<Box<dyn Future<Output = Result<Response<Body>, hyper::Error>> + Send>>;

pub trait HttpClient {
    fn request(&self, req: Request<Body>) -> ResponseFuture;
}

pub struct HyperClient {
    inner: crate::HttpClient,
}

impl HyperClient {
    pub fn new(inner: crate::HttpClient) -> Self {
        Self { inner }
    }
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

impl TryFrom<&Endpoint> for MockClient {
    type Error = anyhow::Error;

    fn try_from(value: &Endpoint) -> Result<Self, Self::Error> {
        match value.url.scheme_str() {
            Some("file") => {
                let file_path = crate::decode_uri_path_in_authority(&value.url)
                    .expect("file urls should always have been encoded in authority");
                Ok(Self {
                    file: Arc::new(Mutex::new(Box::new(
                        File::create(file_path).expect("Couldn't open mock client file"),
                    ))),
                })
            }
            _ => anyhow::bail!("MockClient only supports file:// URLs"),
        }
    }
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
    use crate::HttpRequestBuilder;

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
