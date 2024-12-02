// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use hyper::http;

use hex::FromHex;
use http::{Request, Response, Uri};
use hyper::body::{Body, Incoming};
use hyper::rt::{Read, Write};
use hyper_util::rt::TokioIo;
use std::result::Result as StdResult;
use std::{io, path, sync, time};
use tokio::net::TcpStream;
use tokio::time::error::Elapsed;
use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_util::sync::CancellationToken;

pub trait UriExt {
    fn from_path<S, P>(scheme: S, path: P) -> http::Result<Uri>
    where
        http::uri::Scheme: TryFrom<S>,
        <http::uri::Scheme as TryFrom<S>>::Error: Into<http::Error>,
        P: AsRef<path::Path>;
}

impl UriExt for Uri {
    /// Encode the [path::Path] into a URI with the provided scheme. Since file
    /// system paths are not valid "authority"s in URIs, the path is
    /// hex-encoded.
    fn from_path<S, P>(scheme: S, path: P) -> http::Result<Uri>
    where
        http::uri::Scheme: TryFrom<S>,
        <http::uri::Scheme as TryFrom<S>>::Error: Into<http::Error>,
        P: AsRef<path::Path>,
    {
        let path = path.as_ref();
        let hex_encoded_path = {
            // On Unix we can convert the Path's OsStr into &[u8] using a
            // trait from the prelude. This is possible because OsStr on Unix
            // are basically just byte strings anyway.
            #[cfg(unix)]
            {
                use std::os::unix::prelude::*;
                hex::encode(path.as_os_str().as_bytes())
            }

            #[cfg(not(unix))]
            // But on other platforms, notably Windows, there is not an API
            // for this. So we have to either convert it to UTF lossily, or
            // panic, or handle in some other way the conversion.
            // This chooses to panic because that's what the implementation in
            // ddcommon did.
            {
                hex::encode(path.to_str().unwrap().as_bytes())
            }
        };
        Uri::builder()
            .scheme(scheme)
            .authority(hex_encoded_path)
            .build()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Dns(#[from] rustls::pki_types::InvalidDnsNameError),

    #[error(transparent)]
    Http(#[from] http::Error),

    #[error(transparent)]
    Hyper(#[from] hyper::Error),

    #[error(transparent)]
    Io(#[from] io::Error),

    #[error("secure connections require ClientConfig, none provided")]
    MissingConfig,

    #[error(transparent)]
    Rustls(#[from] rustls::Error),

    #[error(transparent)]
    Timeout(#[from] Elapsed),

    #[error("unsupported scheme: `{0}`")]
    UnsupportedScheme(String),

    #[error("user requested cancellation")]
    UserRequestedCancellation,
}

pub struct Client {
    config: Option<sync::Arc<rustls::ClientConfig>>,
    runtime: tokio::runtime::Runtime,
}

impl Client {
    /// Create a client from the given config and runtime. For do-it-yourself
    /// types and testing.
    pub const fn new(
        config: Option<sync::Arc<rustls::ClientConfig>>,
        runtime: tokio::runtime::Runtime,
    ) -> Self {
        Self { config, runtime }
    }

    /// Create a client with the native cert store and use a current thread
    /// runtime.
    #[cfg(feature = "use_native_roots")]
    pub fn use_native_roots_on_current_thread() -> Result<Self, Error> {
        let provider = crate::crytpo::Provider::use_native_roots();
        let config = provider.get_client_config();
        let runtime = crate::rt::create_current_thread_runtime()?;
        Ok(Client { config, runtime })
    }

    /// Create a client with the webpki certs and use a current thread runtime.
    #[cfg(feature = "use_webpki_roots")]
    pub fn use_webpki_roots_on_current_thread() -> Result<Self, Error> {
        let provider = crate::crytpo::Provider::use_webpki_roots();
        let config = provider.get_client_config();
        let runtime = crate::rt::create_current_thread_runtime()?;
        Ok(Client { config, runtime })
    }

    pub fn send<B>(
        &self,
        request: Request<B>,
        cancel: Option<&CancellationToken>,
        timeout: Option<time::Duration>,
    ) -> Result<Response<Incoming>, Error>
    where
        B: Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let client_config = self.config.clone();
        self.runtime.block_on(async move {
            tokio::select! {
                result = async { match timeout {
                    Some(t) => {
                        tokio::time::timeout(t, send_and_infer_connector(client_config, request))
                            .await?
                    }
                    None => send_and_infer_connector(client_config, request).await,
                }} => result,
                _ = async { match cancel {
                    Some(token) => token.cancelled().await,
                    // If no token is provided, future::pending() provides a
                    // no-op future that never resolves.
                    None => std::future::pending().await,
                }} => Err(Error::UserRequestedCancellation),
            }
        })
    }
}

pub async fn send_and_infer_connector<B>(
    client_config: Option<sync::Arc<rustls::ClientConfig>>,
    request: Request<B>,
) -> StdResult<Response<Incoming>, Error>
where
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let uri = request.uri();
    match uri.scheme() {
        None => Err(Error::UnsupportedScheme(String::new())),
        Some(scheme) => match scheme.as_str() {
            "http" => send_http(request).await,
            "https" => {
                if let Some(client_config) = client_config {
                    send_https(client_config, request).await
                } else {
                    Err(Error::MissingConfig)
                }
            }
            #[cfg(unix)]
            "unix" => send_via_unix_socket(request).await,
            #[cfg(windows)]
            "windows" => send_via_named_pipe(request).await,
            scheme => Err(Error::UnsupportedScheme(String::from(scheme))),
        },
    }
}

#[cfg(unix)]
pub async fn send_via_unix_socket<B>(request: Request<B>) -> StdResult<Response<Incoming>, Error>
where
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let path = parse_path_from_uri(request.uri())?;
    let unix_stream = tokio::net::UnixStream::connect(path).await?;
    let hyper_wrapper = TokioIo::new(unix_stream);

    Ok(send_via_io(request, hyper_wrapper).await?)
}

#[cfg(windows)]
pub async fn send_via_named_pipe<B>(request: Request<B>) -> StdResult<Response<Incoming>, Error>
where
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let _path = parse_path_from_uri(request.uri())?;
    todo!("re-implement named pipes on Windows")
}

pub async fn send_http<B>(request: Request<B>) -> StdResult<Response<Incoming>, Error>
where
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    // This _should_ be a redundant check, caller should only call this if
    // they expect it's an http connection to begin with.
    let scheme_str = request.uri().scheme_str();
    if scheme_str != Some("http") {
        let base = "URI scheme must be http";
        let msg = if let Some(scheme) = scheme_str {
            format!("{base}, given {scheme}")
        } else {
            format!("{base}, empty scheme found")
        };
        let err = io::Error::new(io::ErrorKind::InvalidInput, msg);
        return Err(Error::from(err));
    }

    let authority = request
        .uri()
        .authority()
        .ok_or(Error::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "URI must have host",
        )))?
        .as_str();
    let stream = TcpStream::connect(authority).await?;
    let hyper_wrapper = TokioIo::new(stream);

    Ok(send_via_io(request, hyper_wrapper).await?)
}

pub async fn send_https<B>(
    client_config: sync::Arc<rustls::ClientConfig>,
    request: Request<B>,
) -> StdResult<Response<Incoming>, Error>
where
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let uri = request.uri();

    // This _should_ be a redundant check, caller should only call this if
    // they expect it's an https connection to begin with.
    let scheme_str = request.uri().scheme_str();
    if scheme_str != Some("https") {
        let base = "URI scheme must be https";
        let msg = if let Some(scheme) = scheme_str {
            format!("{base}, given {scheme}")
        } else {
            format!("{base}, empty scheme found")
        };
        let err = io::Error::new(io::ErrorKind::InvalidInput, msg);
        return Err(Error::from(err));
    }

    let server_name = ServerName::try_from(uri.to_string())?;
    let connector = tokio_rustls::TlsConnector::from(client_config);

    let tcp_stream = {
        let authority = uri.authority().ok_or(Error::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "URI must have a host",
        )))?;
        TcpStream::connect(authority.as_str()).await?
    };

    let stream = connector.connect(server_name, tcp_stream).await?;
    let hyper_wrapper = TokioIo::new(stream);
    Ok(send_via_io(request, hyper_wrapper).await?)
}

async fn send_via_io<T, B>(
    request: Request<B>,
    io: T,
) -> StdResult<Response<Incoming>, hyper::Error>
where
    T: Read + Write + Send + Unpin + 'static,
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io).await?;

    // The docs say we need to poll this to drive it to completion, but they
    // never directly use the return type or anything:
    // https://hyper.rs/guides/1/client/basic/
    let _todo = tokio::spawn(connection);

    sender.send_request(request).await
}

pub fn parse_path_from_uri(uri: &Uri) -> io::Result<path::PathBuf> {
    // This _should_ be a redundant check, caller should only call this if
    // they expect it's a unix domain socket or windows named pipe.
    let scheme_str = uri.scheme_str();
    if scheme_str != Some("unix") || scheme_str != Some("windows") {
        let base = "URI scheme must be unix or windows";
        let msg = if let Some(scheme) = scheme_str {
            format!("{base}, given {scheme}")
        } else {
            format!("{base}, empty scheme found")
        };
        return Err(io::Error::new(io::ErrorKind::InvalidInput, msg));
    }

    if let Some(host) = uri.host() {
        let bytes = Vec::from_hex(host).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("URI host must be a hex-encoded path: {err}"),
            )
        })?;
        let str = match std::str::from_utf8(&bytes) {
            Ok(s) => s,
            Err(err) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("URI is invalid: {err}"),
                ))
            }
        };
        Ok(path::PathBuf::from(str))
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "URI is missing host",
        ))
    }
}
