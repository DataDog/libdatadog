// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::{future, Future, FutureExt, TryFutureExt};
use hyper_rustls::HttpsConnector;
use pin_project::pin_project;

#[derive(Debug)]
#[pin_project(project=ConnStreamProj)]
pub enum ConnStream {
    Tcp {
        #[pin]
        transport: tokio::net::TcpStream,
    },
    Tls {
        #[pin]
        transport: Box<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>,
    },
    #[cfg(unix)]
    Udp {
        #[pin]
        transport: tokio::net::UnixStream,
    },
}

pub type ConnStreamError = Box<dyn std::error::Error + Send + Sync>;

use hyper::{client::HttpConnector, service::Service};
impl ConnStream {
    pub async fn from_uds_uri(uri: hyper::Uri) -> Result<ConnStream, ConnStreamError> {
        #[cfg(unix)]
        {
            let path = super::uds::socket_path_from_uri(&uri)?;
            Ok(ConnStream::Udp {
                transport: tokio::net::UnixStream::connect(path).await?,
            })
        }
        #[cfg(not(unix))]
        {
            Err(crate::errors::Error::UnixSockeUnsuported.into())
        }
    }

    pub fn from_http_connector_with_uri(
        c: &mut HttpConnector,
        uri: hyper::Uri,
    ) -> impl Future<Output = Result<ConnStream, ConnStreamError>> {
        c.call(uri).map(|r| match r {
            Ok(t) => Ok(ConnStream::Tcp { transport: t }),
            Err(e) => Err(e.into()),
        })
    }

    pub fn from_https_connector_with_uri(
        c: &mut HttpsConnector<HttpConnector>,
        uri: hyper::Uri,
        require_tls: bool,
    ) -> impl Future<Output = Result<ConnStream, ConnStreamError>> {
        c.call(uri).and_then(move |stream| match stream {
            // move only require_tls
            hyper_rustls::MaybeHttpsStream::Http(t) => {
                if require_tls {
                    future::ready(Err(
                        crate::errors::Error::CannotEstablishTlsConnection.into()
                    ))
                } else {
                    future::ready(Ok(ConnStream::Tcp { transport: t }))
                }
            }
            hyper_rustls::MaybeHttpsStream::Https(t) => future::ready(Ok(ConnStream::Tls {
                transport: Box::from(t),
            })),
        })
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
            ConnStreamProj::Tls { transport } => transport.poll_read(cx, buf),
            #[cfg(unix)]
            ConnStreamProj::Udp { transport } => transport.poll_read(cx, buf),
        }
    }
}

impl hyper::client::connect::Connection for ConnStream {
    fn connected(&self) -> hyper::client::connect::Connected {
        match self {
            Self::Tcp { transport } => transport.connected(),
            Self::Tls { transport } => {
                let (tcp, _) = transport.get_ref();
                tcp.connected()
            }
            #[cfg(unix)]
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
            ConnStreamProj::Tls { transport } => transport.poll_write(cx, buf),
            #[cfg(unix)]
            ConnStreamProj::Udp { transport } => transport.poll_write(cx, buf),
        }
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        match self.project() {
            ConnStreamProj::Tcp { transport } => transport.poll_shutdown(cx),
            ConnStreamProj::Tls { transport } => transport.poll_shutdown(cx),
            #[cfg(unix)]
            ConnStreamProj::Udp { transport } => transport.poll_shutdown(cx),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        match self.project() {
            ConnStreamProj::Tcp { transport } => transport.poll_flush(cx),
            ConnStreamProj::Tls { transport } => transport.poll_flush(cx),
            #[cfg(unix)]
            ConnStreamProj::Udp { transport } => transport.poll_flush(cx),
        }
    }
}
