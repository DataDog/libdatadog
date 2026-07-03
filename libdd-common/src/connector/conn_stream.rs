// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::{Future, FutureExt};
use hyper::rt::ReadBufCursor;
use pin_project::pin_project;

#[pin_project(project=ConnStreamProj)]
#[derive(Debug)]
pub enum ConnStream {
    Tcp {
        #[pin]
        transport: TokioIo<tokio::net::TcpStream>,
    },
    #[cfg(feature = "tls-core")]
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

pub type ConnStreamError = Box<dyn core::error::Error + Send + Sync>;

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
                    tokio::net::windows::named_pipe::ClientOptions::new()
                        .security_qos_flags(super::named_pipe::ANONYMOUS_IMPERSONATION_QOS)
                        .open(path)?,
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
    ) -> impl Future<Output = Result<ConnStream, ConnStreamError>> + 'static {
        c.call(uri).map(|r| match r {
            Ok(t) => Ok(ConnStream::Tcp { transport: t }),
            Err(e) => Err(e.into()),
        })
    }

    #[cfg(feature = "tls-core")]
    pub fn from_https_connector_with_uri(
        c: &mut hyper_rustls::HttpsConnector<connect::HttpConnector>,
        uri: hyper::Uri,
        require_tls: bool,
    ) -> impl Future<Output = Result<ConnStream, ConnStreamError>> + 'static {
        #[allow(clippy::unwrap_used)]
        let stream_fut = c.call(uri.to_string().parse().unwrap());
        async move {
            let stream = stream_fut.await?;
            match stream {
                // move only require_tls
                hyper_rustls::MaybeHttpsStream::Http(t) => {
                    if require_tls {
                        Err(super::errors::Error::CannotEstablishTlsConnection.into())
                    } else {
                        Ok(ConnStream::Tcp { transport: t })
                    }
                }
                hyper_rustls::MaybeHttpsStream::Https(t) => Ok(ConnStream::Tls {
                    transport: Box::from(t),
                }),
            }
        }
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
            #[cfg(feature = "tls-core")]
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
            #[cfg(feature = "tls-core")]
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
            #[cfg(feature = "tls-core")]
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
            #[cfg(feature = "tls-core")]
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
            #[cfg(feature = "tls-core")]
            ConnStreamProj::Tls { transport } => transport.poll_flush(cx),
            #[cfg(unix)]
            ConnStreamProj::Udp { transport } => transport.poll_flush(cx),
            #[cfg(windows)]
            ConnStreamProj::NamedPipe { transport } => transport.poll_flush(cx),
        }
    }
}

#[cfg(all(test, windows))]
mod windows_named_pipe_tests {
    use super::ConnStream;
    use crate::connector::named_pipe::named_pipe_path_to_uri;
    use std::path::Path;
    use tokio::net::windows::named_pipe::ServerOptions;

    /// Verifies that `from_named_pipe_uri` opens a client connection successfully
    /// when impersonation is disabled (Anonymous QoS). The server accepting the
    /// connection confirms the `SECURITY_SQOS_PRESENT | SECURITY_ANONYMOUS` flags
    /// produce a usable transport.
    #[tokio::test]
    async fn from_named_pipe_uri_connects_with_anonymous_qos() {
        let pipe_name = format!(
            r"\\.\pipe\libdd_common_conn_stream_test_{}_{}",
            std::process::id(),
            rand::random::<u64>()
        );

        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe_name)
            .expect("failed to create named pipe server");

        let server_task = tokio::spawn(async move {
            server.connect().await.expect("server failed to accept");
        });

        let uri = named_pipe_path_to_uri(Path::new(&pipe_name)).expect("failed to build uri");
        let conn = ConnStream::from_named_pipe_uri(uri).await;
        assert!(
            conn.is_ok(),
            "expected named pipe client to connect with Anonymous QoS: {:?}",
            conn.err()
        );

        server_task.await.expect("server task panicked");
    }
}
