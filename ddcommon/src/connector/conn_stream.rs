// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::{future, Future, FutureExt, TryFutureExt};
use hyper_rustls::HttpsConnector;
use pin_project::pin_project;

#[pin_project(project=ConnStreamProj)]
#[derive(Debug)]
pub enum ConnStream {
    Tcp {
        #[pin]
        transport: tokio::net::TcpStream,
    },
    Tls {
        #[pin]
        transport: Box<tokio_rustls::client::TlsStream<TokioIo<TokioIo<tokio::net::TcpStream>>>>,
    },
    #[cfg(unix)]
    Udp {
        #[pin]
        transport: tokio::net::UnixStream,
    },

    #[cfg(windows)]
    NamedPipe {
        #[pin]
        transport: tokio::net::windows::named_pipe::NamedPipeClient,
    },
}

pub type ConnStreamError = Box<dyn std::error::Error + Send + Sync>;

use hyper::{client::HttpConnector, service::Service};
use hyper_util::rt::TokioIo;

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
            let _ = uri;
            Err(super::errors::Error::UnixSocketUnsupported.into())
        }
    }

    pub async fn from_named_pipe_uri(uri: hyper::Uri) -> Result<ConnStream, ConnStreamError> {
        #[cfg(windows)]
        {
            let path = super::named_pipe::named_pipe_path_from_uri(&uri)?;
            Ok(ConnStream::NamedPipe {
                transport: tokio::net::windows::named_pipe::ClientOptions::new().open(path)?,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = uri;
            Err(super::errors::Error::WindowsNamedPipeUnsupported.into())
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
        c: &mut HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        uri: hyper::Uri,
        require_tls: bool,
    ) -> impl Future<Output = Result<ConnStream, ConnStreamError>> {
        c.call(uri.to_string().parse().unwrap())
            .and_then(move |stream| match stream {
                // move only require_tls
                hyper_rustls::MaybeHttpsStream::Http(t) => {
                    if require_tls {
                        future::ready(Err(
                            super::errors::Error::CannotEstablishTlsConnection.into()
                        ))
                    } else {
                        future::ready(Ok(ConnStream::Tcp {
                            transport: t.into_inner(),
                        }))
                    }
                }
                hyper_rustls::MaybeHttpsStream::Https(t) => future::ready(Ok(ConnStream::Tls {
                    transport: Box::from(t.into_inner()),
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
            #[cfg(windows)]
            ConnStreamProj::NamedPipe { transport } => transport.poll_read(cx, buf),
        }
    }
}

impl hyper::client::connect::Connection for ConnStream {
    fn connected(&self) -> hyper::client::connect::Connected {
        match self {
            Self::Tcp { transport } => transport.connected(),
            Self::Tls { transport } => {
                let (tcp, _) = transport.get_ref();
                tcp.inner().inner().connected()
            }
            #[cfg(unix)]
            Self::Udp { transport: _ } => hyper::client::connect::Connected::new(),
            #[cfg(windows)]
            Self::NamedPipe { transport: _ } => hyper::client::connect::Connected::new(),
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
            #[cfg(windows)]
            ConnStreamProj::NamedPipe { transport } => transport.poll_write(cx, buf),
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
            #[cfg(windows)]
            ConnStreamProj::NamedPipe { transport } => transport.poll_shutdown(cx),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        match self.project() {
            ConnStreamProj::Tcp { transport } => transport.poll_flush(cx),
            ConnStreamProj::Tls { transport } => transport.poll_flush(cx),
            #[cfg(unix)]
            ConnStreamProj::Udp { transport } => transport.poll_flush(cx),
            #[cfg(windows)]
            ConnStreamProj::NamedPipe { transport } => transport.poll_flush(cx),
        }
    }
}
