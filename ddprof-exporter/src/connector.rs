use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use pin_project_lite::pin_project;

#[derive(Clone)]
struct UnixConnector();

impl hyper::service::Service<hyper::Uri> for UnixConnector {
    type Response = tokio::net::UnixStream;
    type Error = Box<dyn Error + Sync + Send>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&mut self, uri: hyper::Uri) -> Self::Future {
        Box::pin(async move { Ok(tokio::net::UnixStream::connect(uri.path()).await?) })
    }

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

#[derive(Clone)]
pub struct Connector {
    tcp: hyper_rustls::HttpsConnector<hyper::client::HttpConnector>,
}

impl Connector {
    pub(crate) fn new() -> Self {
        Self {
            tcp: hyper_rustls::HttpsConnectorBuilder::new()
                .with_native_roots()
                .https_or_http()
                .enable_http1()
                .build(),
        }
    }
}

pin_project! {
    #[project = ConnStreamProj]
    pub enum ConnStream {
        Tcp{ #[pin] transport: hyper_rustls::MaybeHttpsStream<tokio::net::TcpStream> },
        Udp{ #[pin] transport: tokio::net::UnixStream },
    }
}

impl tokio::io::AsyncRead for ConnStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.project() {
            ConnStreamProj::Tcp { transport } => transport.poll_read(cx, buf),
            ConnStreamProj::Udp { transport } => transport.poll_read(cx, buf),
        }
    }
}

impl hyper::client::connect::Connection for ConnStream {
    fn connected(&self) -> hyper::client::connect::Connected {
        match self {
            Self::Tcp { transport } => transport.connected(),
            Self::Udp { transport: _ } => hyper::client::connect::Connected::new(),
        }
    }
}

impl tokio::io::AsyncWrite for ConnStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        match self.project() {
            ConnStreamProj::Tcp { transport } => transport.poll_write(cx, buf),
            ConnStreamProj::Udp { transport } => transport.poll_write(cx, buf),
        }
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        match self.project() {
            ConnStreamProj::Tcp { transport } => transport.poll_shutdown(cx),
            ConnStreamProj::Udp { transport } => transport.poll_shutdown(cx),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        match self.project() {
            ConnStreamProj::Tcp { transport } => transport.poll_flush(cx),
            ConnStreamProj::Udp { transport } => transport.poll_flush(cx),
        }
    }
}

impl hyper::service::Service<hyper::Uri> for Connector {
    type Response = ConnStream;
    type Error = Box<dyn Error + Sync + Send>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&mut self, uri: hyper::Uri) -> Self::Future {
        match uri.scheme_str() {
            Some("unix") => Box::pin(async move {
                Ok(ConnStream::Udp {
                    transport: tokio::net::UnixStream::connect(uri.path()).await?,
                })
            }),
            _ => {
                let fut = self.tcp.call(uri);
                Box::pin(async {
                    Ok(ConnStream::Tcp {
                        transport: fut.await?,
                    })
                })
            }
        }
    }

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.tcp.poll_ready(cx).map_err(|e| e.into())
    }
}

#[test]
fn test_hyper_client_from_connector() {
    let _: hyper::Client<Connector> = hyper::Client::builder().build(Connector::new());
}
