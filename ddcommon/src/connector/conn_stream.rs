// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::{future, Future, FutureExt, TryFutureExt};
use hyper::rt::ReadBufCursor;
use hyper_rustls::HttpsConnector;
use pin_project::pin_project;

#[pin_project(project=ConnStreamProj)]
#[derive(Debug)]
pub enum ConnStream {
    Tcp {
        #[pin]
        transport: TokioIo<tokio::net::TcpStream>,
    },
    #[cfg(feature = "https")]
    Tls {
        #[pin]
        transport:
            Box<TokioIo<tokio_rustls::client::TlsStream<TokioIo<TokioIo<tokio::net::TcpStream>>>>>,
    },
    #[cfg(unix)]
    Udp {
        #[pin]
        transport: TokioIo<tokio::net::UnixStream>,
    },
    #[cfg(windows)]
    NamedPipe {
        #[pin]
        transport: TokioIo<tokio::net::windows::named_pipe::NamedPipeClient>,
    },
}

pub type ConnStreamError = Box<dyn std::error::Error + Send + Sync>;

use hyper_util::client::legacy::connect::{self, HttpConnector};
use hyper_util::rt::TokioIo;
use tower_service::Service;

impl ConnStream {
    pub async fn from_uds_uri(uri: hyper::Uri) -> Result<ConnStream, ConnStreamError> {
        #[cfg(unix)]
        {
            let path = super::uds::socket_path_from_uri(&uri)?;
            Ok(ConnStream::Udp {
                transport: TokioIo::new(tokio::net::UnixStream::connect(path).await?),
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
                transport: TokioIo::new(
                    tokio::net::windows::named_pipe::ClientOptions::new().open(path)?,
                ),
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

    #[cfg(feature = "https")]
    pub fn from_https_connector_with_uri(
        c: &mut HttpsConnector<connect::HttpConnector>,
        uri: hyper::Uri,
        require_tls: bool,
    ) -> impl Future<Output = Result<ConnStream, ConnStreamError>> {
        #[allow(clippy::unwrap_used)]
        c.call(uri.to_string().parse().unwrap())
            .and_then(move |stream| match stream {
                // move only require_tls
                hyper_rustls::MaybeHttpsStream::Http(t) => {
                    if require_tls {
                        future::ready(Err(
                            super::errors::Error::CannotEstablishTlsConnection.into()
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

impl hyper::rt::Read for ConnStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.project() {
            ConnStreamProj::Tcp { transport } => transport.poll_read(cx, buf),
            #[cfg(feature = "https")]
            ConnStreamProj::Tls { transport } => transport.poll_read(cx, buf),
            #[cfg(unix)]
            ConnStreamProj::Udp { transport } => transport.poll_read(cx, buf),
            #[cfg(windows)]
            ConnStreamProj::NamedPipe { transport } => transport.poll_read(cx, buf),
        }
    }
}

impl connect::Connection for ConnStream {
    fn connected(&self) -> connect::Connected {
        match self {
            Self::Tcp { transport } => transport.connected(),
            #[cfg(feature = "https")]
            Self::Tls { transport } => {
                let (tcp, _) = transport.inner().get_ref();
                tcp.inner().inner().connected()
            }
            #[cfg(unix)]
            Self::Udp { transport: _ } => connect::Connected::new(),
            #[cfg(windows)]
            Self::NamedPipe { transport: _ } => connect::Connected::new(),
        }
    }
}

impl hyper::rt::Write for ConnStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        match self.project() {
            ConnStreamProj::Tcp { transport } => transport.poll_write(cx, buf),
            #[cfg(feature = "https")]
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
            #[cfg(feature = "https")]
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
            #[cfg(feature = "https")]
            ConnStreamProj::Tls { transport } => transport.poll_flush(cx),
            #[cfg(unix)]
            ConnStreamProj::Udp { transport } => transport.poll_flush(cx),
            #[cfg(windows)]
            ConnStreamProj::NamedPipe { transport } => transport.poll_flush(cx),
        }
    }
}
