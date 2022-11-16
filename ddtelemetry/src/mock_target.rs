use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;

use bytes::Buf;
use futures::{future, Future};
use http::Uri;
use hyper::server::conn::{AddrIncoming, AddrStream};
use hyper::service::{make_service_fn, service_fn, Service};
use hyper::{header, Body, Method, Request, Response, Server, StatusCode};
use tarpc::tokio_util::sync::CancellationToken;

pub struct MockServer {
    local_addr: SocketAddr,
    cancellation_token: CancellationToken,
}

impl MockServer {
    pub async fn start_random_local_port() -> anyhow::Result<Self> {
        let addr = "127.0.0.1:0".parse().unwrap();
        let server = Server::bind(&addr).serve(make_service_fn(|_| async move {
            Ok::<_, Infallible>(service_fn(move |r: Request<Body>| async move {
                println!("{:?}", r);
                Ok::<_, Infallible>(Response::new(Body::from("Hello!")))
            }))
        }));
        let cancellation_token = CancellationToken::default();

        let local_addr = server.local_addr();
        let token = cancellation_token.clone();

        tokio::spawn(async move {
            tokio::select! {
                _ = token.cancelled() => {},
                _ = server => {
                }
            }
        });

        Ok(Self {
            local_addr,
            cancellation_token,
        })
    }

    pub fn get_url(&self) -> Uri {
        Uri::builder()
            .scheme("http")
            .authority(format!("127.0.0.1:{}", self.local_addr.port()))
            .build()
            .unwrap_or_default()
    }
    pub fn shutdown(&self) {
        self.cancellation_token.cancel()
    }
}

pub fn start_mock_server() -> anyhow::Result<()> {
    Ok(())
}
