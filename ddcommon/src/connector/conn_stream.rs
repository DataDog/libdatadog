// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::{Future, FutureExt};
use pin_project::pin_project;

#[cfg(any(feature = "rustls", feature = "native-tls"))]
use futures::future;

#[pin_project(project=ConnStreamProj)]
#[derive(Debug)]
pub enum ConnStream {
    Tcp {
        #[pin]
        transport: tokio::net::TcpStream,
    },
    #[cfg(feature = "rustls")]
    Rtls {
        #[pin]
        transport: Box<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>,
    },
    #[cfg(feature = "native-tls")]
    Ntls {
        #[pin]
        transport: Box<hyper_tls::TlsStream<tokio::net::TcpStream>>,
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

    #[cfg(feature = "rustls")]
    pub fn from_https_connector_with_uri_rtls(
        c: &mut hyper_rustls::HttpsConnector<HttpConnector>,
        uri: hyper::Uri,
        require_tls: bool,
    ) -> impl Future<Output = Result<ConnStream, ConnStreamError>> {
        use futures::TryFutureExt;
        c.call(uri).and_then(move |stream| match stream {
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
            hyper_rustls::MaybeHttpsStream::Https(t) => future::ready(Ok(ConnStream::Rtls {
                transport: Box::from(t),
            })),
        })
    }
    #[cfg(feature = "native-tls")]
    pub fn from_https_connector_with_uri_ntls(
        c: &mut hyper_tls::HttpsConnector<HttpConnector>,
        uri: hyper::Uri,
        require_tls: bool,
    ) -> impl Future<Output = Result<ConnStream, ConnStreamError>> {
        use futures::TryFutureExt;
        c.call(uri).and_then(move |stream| match stream {
            // move only require_tls
            hyper_tls::MaybeHttpsStream::Http(t) => {
                if require_tls {
                    future::ready(Err(
                        super::errors::Error::CannotEstablishTlsConnection.into()
                    ))
                } else {
                    future::ready(Ok(ConnStream::Tcp { transport: t }))
                }
            }
            hyper_tls::MaybeHttpsStream::Https(t) => future::ready(Ok(ConnStream::Ntls {
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
            #[cfg(feature = "rustls")]
            ConnStreamProj::Rtls { transport } => transport.poll_read(cx, buf),
            #[cfg(feature = "native-tls")]
            ConnStreamProj::Ntls { transport } => transport.poll_read(cx, buf),
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
            #[cfg(feature = "rustls")]
            Self::Rtls { transport } => {
                let (tcp, _) = transport.get_ref();
                tcp.connected()
            }
            #[cfg(feature = "native-tls")]
            Self::Ntls { transport } => {
                let tcp = transport.get_ref().get_ref().get_ref();
                tcp.connected()
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
            #[cfg(feature = "rustls")]
            ConnStreamProj::Rtls { transport } => transport.poll_write(cx, buf),
            #[cfg(feature = "native-tls")]
            ConnStreamProj::Ntls { transport } => transport.poll_write(cx, buf),
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
            #[cfg(feature = "rustls")]
            ConnStreamProj::Rtls { transport } => transport.poll_shutdown(cx),
            #[cfg(feature = "native-tls")]
            ConnStreamProj::Ntls { transport } => transport.poll_shutdown(cx),
            #[cfg(unix)]
            ConnStreamProj::Udp { transport } => transport.poll_shutdown(cx),
            #[cfg(windows)]
            ConnStreamProj::NamedPipe { transport } => transport.poll_shutdown(cx),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        match self.project() {
            ConnStreamProj::Tcp { transport } => transport.poll_flush(cx),
            #[cfg(feature = "rustls")]
            ConnStreamProj::Rtls { transport } => transport.poll_flush(cx),
            #[cfg(feature = "native-tls")]
            ConnStreamProj::Ntls { transport } => transport.poll_flush(cx),
            #[cfg(unix)]
            ConnStreamProj::Udp { transport } => transport.poll_flush(cx),
            #[cfg(windows)]
            ConnStreamProj::NamedPipe { transport } => transport.poll_flush(cx),
        }
    }
}
