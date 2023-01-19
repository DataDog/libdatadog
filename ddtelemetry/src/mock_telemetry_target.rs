use std::convert::Infallible;
use std::net::SocketAddr;

use std::thread;


use futures::future::{BoxFuture, Shared};
use futures::{FutureExt};
use http::Uri;

use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};
use tarpc::tokio_util::sync::CancellationToken;

pub struct MockServer {
    local_addr: SocketAddr,
    cancellation_token: CancellationToken,
    shutdown_future: Shared<BoxFuture<'static, Option<()>>>,
}

impl MockServer {
    pub fn start_random_local_port() -> anyhow::Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let res = rt.block_on(Self::async_start_random_local_port())?;
        let shutdown_future = res.shutdown_future.clone();
        thread::spawn(move || {
            // make the runtime start executing
            rt.block_on(shutdown_future);
        });

        Ok(res)
    }

    pub async fn async_start_random_local_port() -> anyhow::Result<Self> {
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

        let shutdown_future = tokio::spawn(async move {
            tokio::select! {
                _ = token.cancelled() => {},
                _ = server => {
                }
            }
        });

        Ok(Self {
            local_addr,
            cancellation_token,
            shutdown_future: shutdown_future.map(Result::ok).boxed().shared()
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
