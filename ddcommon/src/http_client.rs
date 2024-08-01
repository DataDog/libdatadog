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

pub fn from_endpoint(endpoint_opt: &Option<Endpoint>) -> Box<dyn HttpClient + Sync + Send> {
    match &endpoint_opt {
        Some(e) if e.url.scheme_str() == Some("file") => {
            let file_path = crate::decode_uri_path_in_authority(&e.url)
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
            .build(crate::connector::Connector::default()),
    })
}

pub struct HyperClient {
    inner: crate::HttpClient,
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
