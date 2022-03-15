use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

// Tokio doesn't handle unix sockets on windows
#[cfg(unix)]
pub(crate) mod uds {
    use pin_project_lite::pin_project;
    use std::error::Error;
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};

    /// Creates a new Uri, with the `unix` scheme, and the path to the socket
    /// encoded as a hex string, to prevent special characters in the url authority
    pub fn socket_path_to_uri(path: &Path) -> Result<hyper::Uri, Box<dyn Error>> {
        let path = hex::encode(path.as_os_str().as_bytes());
        Ok(hyper::Uri::builder()
            .scheme("unix")
            .authority(path)
            .path_and_query("")
            .build()?)
    }

    pub fn socket_path_from_uri(
        uri: &hyper::Uri,
    ) -> Result<PathBuf, Box<dyn Error + Sync + Send + 'static>> {
        if uri.scheme_str() != Some("unix") {
            return Err(crate::errors::Error::InvalidUrl.into());
        }
        let path = hex::decode(
            uri.authority()
                .ok_or(crate::errors::Error::InvalidUrl)?
                .as_str(),
        )
        .map_err(|_| crate::errors::Error::InvalidUrl)?;
        Ok(PathBuf::from(OsString::from_vec(path)))
    }

    #[test]
    fn test_encode_unix_socket_path_absolute() {
        let expected_path = "/path/to/a/socket.sock".as_ref();
        let uri = socket_path_to_uri(expected_path).unwrap();
        assert_eq!(uri.scheme_str(), Some("unix"));

        let actual_path = socket_path_from_uri(&uri).unwrap();
        assert_eq!(actual_path.as_path(), Path::new(expected_path))
    }

    #[test]
    fn test_encode_unix_socket_relative_path() {
        let expected_path = "relative/path/to/a/socket.sock".as_ref();
        let uri = socket_path_to_uri(expected_path).unwrap();
        let actual_path = socket_path_from_uri(&uri).unwrap();
        assert_eq!(actual_path.as_path(), Path::new(expected_path));

        let expected_path = "./relative/path/to/a/socket.sock".as_ref();
        let uri = socket_path_to_uri(expected_path).unwrap();
        let actual_path = socket_path_from_uri(&uri).unwrap();
        assert_eq!(actual_path.as_path(), Path::new(expected_path));
    }

    pin_project! {
        #[project = ConnStreamProj]
        pub(crate) enum ConnStream {
            Tcp{ #[pin] transport: hyper_rustls::MaybeHttpsStream<tokio::net::TcpStream> },
            Udp{ #[pin] transport: tokio::net::UnixStream },
        }
    }
}

#[cfg(unix)]
use uds::{ConnStream, ConnStreamProj};

#[cfg(not(unix))]
pin_project_lite::pin_project! {
    #[project = ConnStreamProj]
    pub(crate) enum ConnStream {
        Tcp{ #[pin] transport: hyper_rustls::MaybeHttpsStream<tokio::net::TcpStream> },
    }
}

#[derive(Clone)]
pub(crate) struct Connector {
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

impl tokio::io::AsyncRead for ConnStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.project() {
            ConnStreamProj::Tcp { transport } => transport.poll_read(cx, buf),
            #[cfg(unix)]
            ConnStreamProj::Udp { transport } => transport.poll_read(cx, buf),
        }
    }
}

impl hyper::client::connect::Connection for ConnStream {
    fn connected(&self) -> hyper::client::connect::Connected {
        match self {
            Self::Tcp { transport } => transport.connected(),
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
            #[cfg(unix)]
            ConnStreamProj::Udp { transport } => transport.poll_shutdown(cx),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        match self.project() {
            ConnStreamProj::Tcp { transport } => transport.poll_flush(cx),
            #[cfg(unix)]
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
                #[cfg(unix)]
                {
                    let path = uds::socket_path_from_uri(&uri)?;
                    Ok(ConnStream::Udp {
                        transport: tokio::net::UnixStream::connect(path).await?,
                    })
                }
                #[cfg(not(unix))]
                {
                    Err(crate::errors::Error::UnixSockeUnsuported.into())
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Verify that the Connector type implements the correct bound Connect + Clone
    /// to be able to use the hyper::Client
    fn test_hyper_client_from_connector() {
        let _: hyper::Client<Connector> = hyper::Client::builder().build(Connector::new());
    }
}
