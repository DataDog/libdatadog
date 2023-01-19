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

pub type ResponseFuture =
    Pin<Box<dyn Future<Output = Result<Response<Body>, hyper::Error>> + Send>>;

pub trait HttpClient {
    fn request(&self, req: Request<hyper::Body>) -> ResponseFuture;
}

pub fn from_config(c: &Config) -> Box<dyn HttpClient + Sync + Send> {
    if let Some(ref p) = c.mock_client_file {
        Box::new(MockClient {
            file: Arc::new(Mutex::new(Box::new(
                File::create(p).expect("Couldn't open mock client file"),
            ))),
        })
    } else {
        Box::new(HyperClient {
            inner: c.http_client(),
        })
    }
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
