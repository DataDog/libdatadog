// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

extern crate alloc;

use alloc::borrow::Cow;
use anyhow::Context;
use core::{ops::Deref, str::FromStr};
use http::uri::{self, PathAndQuery, Uri};
use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub mod azure_app_services;
#[cfg(not(target_arch = "wasm32"))]
pub mod cc_utils;
#[cfg(not(target_arch = "wasm32"))]
pub mod connector;
#[cfg(feature = "reqwest")]
pub mod dump_server;
pub mod entity_id;
pub mod machine_id;
pub mod regex_engine;
#[macro_use]
pub mod cstr;
#[cfg(feature = "bench-utils")]
pub mod bench_utils;
pub mod config;
pub mod error;
pub mod http_common;
pub mod multipart;
#[cfg(not(target_arch = "wasm32"))]
pub mod rate_limiter;
pub mod tag;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;
#[cfg(not(target_arch = "wasm32"))]
pub mod threading;
#[cfg(not(target_arch = "wasm32"))]
pub mod timeout;
pub mod unix_utils;

/// Extension trait for `Mutex` to provide a method that acquires a lock, panicking if the lock is
/// poisoned.
///
/// This helper function is intended to be used to avoid having to add many
/// `#[allow(clippy::unwrap_used)]` annotations if there are a lot of usages of `Mutex`.
///
/// # Arguments
///
/// * `self` - A reference to the `Mutex` to lock.
///
/// # Returns
///
/// A `MutexGuard` that provides access to the locked data.
///
/// # Panics
///
/// This function will panic if the `Mutex` is poisoned.
///
/// # Examples
///
/// ```
/// use libdd_common::MutexExt;
/// use std::sync::{Arc, Mutex};
///
/// let data = Arc::new(Mutex::new(5));
/// let data_clone = Arc::clone(&data);
///
/// std::thread::spawn(move || {
///     let mut num = data_clone.lock_or_panic();
///     *num += 1;
/// })
/// .join()
/// .expect("Thread panicked");
///
/// assert_eq!(*data.lock_or_panic(), 6);
/// ```
pub trait MutexExt<T> {
    fn lock_or_panic(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    #[inline(always)]
    #[track_caller]
    fn lock_or_panic(&self) -> MutexGuard<'_, T> {
        #[allow(clippy::unwrap_used)]
        self.lock().unwrap()
    }
}

/// Extension trait for `RwLock` to provide methods that acquire read/write locks, panicking if
/// the lock is poisoned.
///
/// Mirrors [`MutexExt`] for `RwLock` so callers avoid `#[allow(clippy::unwrap_used)]` at each
/// lock site.
///
/// # Examples
///
/// ```
/// use libdd_common::RwLockExt;
/// use std::sync::{Arc, RwLock};
///
/// let data = Arc::new(RwLock::new(5));
/// let data_clone = Arc::clone(&data);
///
/// std::thread::spawn(move || {
///     let mut num = data_clone.write_or_panic();
///     *num += 1;
/// })
/// .join()
/// .expect("Thread panicked");
///
/// assert_eq!(*data.read_or_panic(), 6);
/// ```
pub trait RwLockExt<T> {
    fn read_or_panic(&self) -> RwLockReadGuard<'_, T>;
    fn write_or_panic(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T> RwLockExt<T> for RwLock<T> {
    #[inline(always)]
    #[track_caller]
    fn read_or_panic(&self) -> RwLockReadGuard<'_, T> {
        #[allow(clippy::unwrap_used)]
        self.read().unwrap()
    }

    #[inline(always)]
    #[track_caller]
    fn write_or_panic(&self) -> RwLockWriteGuard<'_, T> {
        #[allow(clippy::unwrap_used)]
        self.write().unwrap()
    }
}

/// Extension trait that extracts the value from a `Result` whose error type is uninhabited.
///
/// The signature constrains callers at compile time: the method is only available when the
/// error type is [`core::convert::Infallible`]. No panics — the compiler proves the `Err`
/// arm unreachable from the type.
///
/// # Examples
///
/// ```
/// use libdd_common::ResultInfallibleExt;
/// use std::convert::Infallible;
///
/// let result: Result<i32, Infallible> = Ok(42);
/// assert_eq!(result.unwrap_infallible(), 42);
/// ```
pub trait ResultInfallibleExt<T>: sealed::Sealed {
    fn unwrap_infallible(self) -> T;
}

impl<T> ResultInfallibleExt<T> for Result<T, core::convert::Infallible> {
    #[inline(always)]
    fn unwrap_infallible(self) -> T {
        match self {
            Ok(value) => value,
            Err(never) => match never {},
        }
    }
}

mod sealed {
    pub trait Sealed {}
    impl<T> Sealed for Result<T, core::convert::Infallible> {}
}

pub mod header {
    #![allow(clippy::declare_interior_mutable_const)]
    use http::{header::HeaderName, HeaderValue};

    pub const APPLICATION_MSGPACK_STR: &str = "application/msgpack";
    pub const APPLICATION_PROTOBUF_STR: &str = "application/x-protobuf";

    pub const DATADOG_CONTAINER_ID: HeaderName = HeaderName::from_static("datadog-container-id");
    pub const DATADOG_ENTITY_ID: HeaderName = HeaderName::from_static("datadog-entity-id");
    pub const DATADOG_EXTERNAL_ENV: HeaderName = HeaderName::from_static("datadog-external-env");
    pub const DATADOG_TRACE_COUNT: HeaderName = HeaderName::from_static("x-datadog-trace-count");
    /// Signal to the agent to send 429 responses when a payload is dropped
    /// If this is not set then the agent will always return a 200 regardless if the payload is
    /// dropped.
    pub const DATADOG_SEND_REAL_HTTP_STATUS: HeaderName =
        HeaderName::from_static("datadog-send-real-http-status");
    pub const DATADOG_API_KEY: HeaderName = HeaderName::from_static("dd-api-key");
    pub const APPLICATION_JSON: HeaderValue = HeaderValue::from_static("application/json");
    pub const APPLICATION_MSGPACK: HeaderValue = HeaderValue::from_static(APPLICATION_MSGPACK_STR);
    pub const APPLICATION_PROTOBUF: HeaderValue =
        HeaderValue::from_static(APPLICATION_PROTOBUF_STR);
    pub const X_DATADOG_TEST_SESSION_TOKEN: HeaderName =
        HeaderName::from_static("x-datadog-test-session-token");
}

#[cfg(not(target_arch = "wasm32"))]
pub type HttpClient = http_common::GenericHttpClient<connector::Connector>;
#[cfg(not(target_arch = "wasm32"))]
pub type HttpResponse = http_common::HttpResponse;
pub type HttpRequestBuilder = http::request::Builder;
#[cfg(not(target_arch = "wasm32"))]
pub trait Connect:
    hyper_util::client::legacy::connect::Connect + Clone + Send + Sync + 'static
{
}
#[cfg(not(target_arch = "wasm32"))]
impl<C: hyper_util::client::legacy::connect::Connect + Clone + Send + Sync + 'static> Connect
    for C
{
}

// Used by tag! macro
pub use const_format;

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Endpoint {
    #[serde(serialize_with = "serialize_uri", deserialize_with = "deserialize_uri")]
    pub url: http::Uri,
    pub api_key: Option<Cow<'static, str>>,
    pub timeout_ms: u64,
    /// Sets X-Datadog-Test-Session-Token header on any request
    pub test_token: Option<Cow<'static, str>>,
    /// Use the system DNS resolver when building the HTTP client. If false, the default
    /// in-process resolver is used.
    #[serde(default)]
    pub use_system_resolver: bool,
}

impl Default for Endpoint {
    fn default() -> Self {
        Endpoint {
            url: http::Uri::default(),
            api_key: None,
            timeout_ms: Self::DEFAULT_TIMEOUT,
            test_token: None,
            use_system_resolver: false,
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct SerializedUri<'a> {
    scheme: Option<Cow<'a, str>>,
    authority: Option<Cow<'a, str>>,
    path_and_query: Option<Cow<'a, str>>,
}

fn serialize_uri<S>(uri: &http::Uri, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let parts = uri.clone().into_parts();
    let uri = SerializedUri {
        scheme: parts.scheme.as_ref().map(|s| Cow::Borrowed(s.as_str())),
        authority: parts.authority.as_ref().map(|s| Cow::Borrowed(s.as_str())),
        path_and_query: parts
            .path_and_query
            .as_ref()
            .map(|s| Cow::Borrowed(s.as_str())),
    };
    uri.serialize(serializer)
}

fn deserialize_uri<'de, D>(deserializer: D) -> Result<http::Uri, D::Error>
where
    D: Deserializer<'de>,
{
    let uri = SerializedUri::deserialize(deserializer)?;
    let mut builder = http::Uri::builder();
    if let Some(v) = uri.authority {
        builder = builder.authority(v.deref());
    }
    if let Some(v) = uri.scheme {
        builder = builder.scheme(v.deref());
    }
    if let Some(v) = uri.path_and_query {
        builder = builder.path_and_query(v.deref());
    }

    builder.build().map_err(Error::custom)
}

/// Converts a human-facing URL string into the internal [`http::Uri`]
/// representation.
///
/// NOTE: the name is misleading. For `http`/`https` this is an ordinary parse,
/// but for the `file`/`unix`/`windows` schemes it *encodes* the path into the
/// URI authority (see `encode_uri_path_in_authority`), so it is a
/// URL-string-to-`Uri` *constructor*, not a pure parser.
///
/// WARNING: this is NOT idempotent for those three schemes. The `Uri` it
/// returns stringifies back to the encoded form (`file://<hex>/`), and feeding
/// that string in again re-encodes it, double-encoding the path. Only ever call
/// this on an original URL string — never on the `.to_string()` of a `Uri` that
/// already came out of here.
///
/// TODO: we should properly handle malformed urls
/// * For windows and unix schemes:
///     * For compatibility reasons with existing implementation this parser stores the encoded path
///       in authority section as there is no existing standard [see](https://github.com/whatwg/url/issues/577)
///       that covers this. We need to pick one hack or another
///     * For windows, interprets everything after windows: as path
///     * For unix, interprets everything after unix:// as path
/// * For file scheme implementation will simply backfill missing authority section
pub fn parse_uri(uri: &str) -> anyhow::Result<http::Uri> {
    if let Some(path) = uri.strip_prefix("unix://") {
        encode_uri_path_in_authority("unix", path)
    } else if let Some(path) = uri.strip_prefix("windows:") {
        encode_uri_path_in_authority("windows", path)
    } else if let Some(path) = uri.strip_prefix("file://") {
        encode_uri_path_in_authority("file", path)
    } else {
        Ok(http::Uri::from_str(uri)?)
    }
}

fn encode_uri_path_in_authority(scheme: &str, path: &str) -> anyhow::Result<http::Uri> {
    let mut parts = uri::Parts::default();
    parts.scheme = uri::Scheme::from_str(scheme).ok();

    let path = hex::encode(path);

    parts.authority = uri::Authority::from_str(path.as_str()).ok();
    parts.path_and_query = Some(uri::PathAndQuery::from_static("/"));
    Ok(http::Uri::from_parts(parts)?)
}

pub fn decode_uri_path_in_authority(uri: &http::Uri) -> anyhow::Result<PathBuf> {
    let path = hex::decode(uri.authority().context("missing uri authority")?.as_str())?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(PathBuf::from(std::ffi::OsString::from_vec(path)))
    }
    #[cfg(not(unix))]
    {
        match String::from_utf8(path) {
            Ok(s) => Ok(PathBuf::from(s.as_str())),
            _ => Err(anyhow::anyhow!("file uri should be utf-8")),
        }
    }
}

impl Endpoint {
    /// Default value for the timeout field in milliseconds.
    pub const DEFAULT_TIMEOUT: u64 = 3_000;

    pub fn agentless(site: &str, api_key: String) -> anyhow::Result<Self> {
        Ok(Self {
            url: Uri::builder()
                .scheme("https")
                .authority(
                    uri::Authority::try_from(site)
                        .with_context(|| format!("dd_site is an invalid url: {site}"))?,
                )
                .path_and_query(PathAndQuery::from_static(""))
                .build()
                .with_context(|| format!("rc url is invalid for site: {site}"))?,
            api_key: Some(api_key.into()),
            timeout_ms: Self::DEFAULT_TIMEOUT,
            test_token: None,
            use_system_resolver: true,
        })
    }

    /// Returns an iterator of optional endpoint-specific headers (api-key, test-token)
    /// as (header_name, header_value) string tuples for any that are available.
    pub fn get_optional_headers(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            self.api_key.as_ref().map(|v| ("dd-api-key", v.as_ref())),
            self.test_token
                .as_ref()
                .map(|v| ("x-datadog-test-session-token", v.as_ref())),
        ]
        .into_iter()
        .flatten()
    }

    /// Apply standard headers (user-agent, api-key, test-token, entity headers) to an
    /// [`http::request::Builder`].
    pub fn set_standard_headers(
        &self,
        mut builder: http::request::Builder,
        user_agent: &str,
    ) -> http::request::Builder {
        builder = builder.header("user-agent", user_agent);
        for (name, value) in self.get_optional_headers() {
            builder = builder.header(name, value);
        }
        for (name, value) in entity_id::get_entity_headers() {
            builder = builder.header(name, value);
        }
        builder
    }

    /// Return a request builder with the following headers:
    /// - User agent
    /// - Api key
    /// - Container Id/Entity Id
    pub fn to_request_builder(&self, user_agent: &str) -> anyhow::Result<HttpRequestBuilder> {
        let mut builder = http::Request::builder()
            .uri(self.url.clone())
            .header(http::header::USER_AGENT, user_agent);

        // Add optional endpoint headers (api-key, test-token)
        for (name, value) in self.get_optional_headers() {
            builder = builder.header(name, value);
        }

        // Add entity-related headers (container-id, entity-id, external-env)
        for (name, value) in entity_id::get_entity_headers() {
            builder = builder.header(name, value);
        }

        Ok(builder)
    }

    #[inline]
    pub fn from_slice(url: &str) -> Endpoint {
        Endpoint {
            #[allow(clippy::unwrap_used)]
            url: parse_uri(url).unwrap(),
            ..Default::default()
        }
    }

    #[inline]
    pub fn from_url(url: http::Uri) -> Endpoint {
        Endpoint {
            url,
            ..Default::default()
        }
    }

    pub fn is_file_endpoint(&self) -> bool {
        self.url.scheme_str() == Some("file")
    }

    /// Set a custom timeout for this endpoint.
    /// If not called, uses the default timeout of 3000ms.
    ///
    /// # Arguments
    /// * `timeout_ms` - Timeout in milliseconds. Pass 0 to use the default timeout (3000ms).
    ///
    /// # Returns
    /// Self with the timeout set, allowing for method chaining
    pub fn with_timeout(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = if timeout_ms == 0 {
            Self::DEFAULT_TIMEOUT
        } else {
            timeout_ms
        };
        self
    }

    /// Use the system DNS resolver when building the reqwest client. Only has effect for
    /// HTTP(S) endpoints.
    pub fn with_system_resolver(mut self, use_system_resolver: bool) -> Self {
        self.use_system_resolver = use_system_resolver;
        self
    }

    /// Creates a reqwest ClientBuilder configured for this endpoint.
    ///
    /// This method handles various endpoint schemes:
    /// - `http`/`https`: Standard HTTP(S) endpoints
    /// - `unix`: Unix domain sockets (Unix only)
    /// - `windows`: Windows named pipes (Windows only)
    /// - `file`: File dump endpoints for debugging (spawns a local server to capture requests)
    ///
    /// The default in-process resolver is used for DNS (fork-safe). To use the system DNS resolver
    /// instead (less fork-safe), set [`Endpoint::use_system_resolver`] to true via
    /// [`Endpoint::with_system_resolver`].
    ///
    /// # Returns
    /// A tuple of (ClientBuilder, request_url) where:
    /// - ClientBuilder is configured with the appropriate transport and timeout
    /// - request_url is the URL string to use for HTTP requests
    ///
    /// # Errors
    /// Returns an error if:
    /// - The endpoint scheme is unsupported
    /// - Path decoding fails
    /// - The dump server fails to start (for file:// scheme)
    #[cfg(feature = "reqwest")]
    pub fn to_reqwest_client_builder(&self) -> anyhow::Result<(reqwest::ClientBuilder, String)> {
        use anyhow::Context;

        // Don't use proxies, as this calls `getenv` which is unsafe and not
        // just in theory. It can cause crashes with PHP where php-fpm's env
        // configuration will mutate the system environment (it doesn't pass
        // it as part of the SAPI env, it changes the actual system env).
        let mut builder = reqwest::Client::builder()
            .timeout(core::time::Duration::from_millis(self.timeout_ms))
            .hickory_dns(!self.use_system_resolver)
            .no_proxy();

        let request_url = match self.url.scheme_str() {
            // HTTP/HTTPS endpoints
            Some("http") | Some("https") => self.url.to_string(),

            // File dump endpoint (debugging) - uses platform-specific local transport
            Some("file") => {
                let output_path = decode_uri_path_in_authority(&self.url)
                    .context("Failed to decode file path from URI")?;
                let socket_or_pipe_path = dump_server::spawn_dump_server(output_path)?;

                // Configure the client to use the local socket/pipe
                #[cfg(unix)]
                {
                    builder = builder.unix_socket(socket_or_pipe_path);
                }
                #[cfg(windows)]
                {
                    builder = builder
                        .windows_named_pipe(socket_or_pipe_path.to_string_lossy().to_string());
                }

                "http://localhost/".to_string()
            }

            // Unix domain sockets
            #[cfg(unix)]
            Some("unix") => {
                use connector::uds::socket_path_from_uri;
                let socket_path = socket_path_from_uri(&self.url)?;
                builder = builder.unix_socket(socket_path);
                format!("http://localhost{}", self.url.path())
            }

            // Windows named pipes
            #[cfg(windows)]
            Some("windows") => {
                use connector::named_pipe::named_pipe_path_from_uri;
                let pipe_path = named_pipe_path_from_uri(&self.url)?;
                builder = builder.windows_named_pipe(pipe_path.to_string_lossy().to_string());
                format!("http://localhost{}", self.url.path())
            }

            // Unsupported schemes
            scheme => anyhow::bail!("Unsupported endpoint scheme: {:?}", scheme),
        };

        Ok((builder, request_url))
    }
}

#[cfg(test)]
mod tests {
    use super::parse_uri;

    /// A scheme prefix with an empty path produces an empty (and therefore
    /// dropped) authority. parsing must reject these as malformed rather
    /// than accept them.
    #[test]
    fn empty_authority_uris_are_rejected() {
        for input in ["unix://", "windows:", "file://"] {
            let result = parse_uri(input);
            assert!(
                result.is_err(),
                "expected {input:?} to be rejected, got {result:?}"
            );
        }
    }
}
