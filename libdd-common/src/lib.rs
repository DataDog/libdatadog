// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

use anyhow::Context;
use hyper::http::uri;
use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::{Mutex, MutexGuard};
use std::{borrow::Cow, ops::Deref, path::PathBuf, str::FromStr};

pub mod azure_app_services;
pub mod capabilities;
pub mod cc_utils;
pub mod connector;
#[cfg(feature = "reqwest")]
pub mod dump_server;
pub mod entity_id;
#[macro_use]
pub mod cstr;
pub mod config;
pub mod error;
pub mod http_common;
pub mod rate_limiter;
pub mod tag;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;
pub mod timeout;
pub mod unix_utils;
pub mod worker;

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

pub mod header {
    #![allow(clippy::declare_interior_mutable_const)]
    use hyper::{header::HeaderName, http::HeaderValue};

    // These strings are defined separately to be used in context where &str are used to represent
    // headers (e.g. SendData) while keeping a single source of truth.
    pub const DATADOG_SEND_REAL_HTTP_STATUS_STR: &str = "datadog-send-real-http-status";
    pub const DATADOG_TRACE_COUNT_STR: &str = "x-datadog-trace-count";
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
        HeaderName::from_static(DATADOG_SEND_REAL_HTTP_STATUS_STR);
    pub const DATADOG_API_KEY: HeaderName = HeaderName::from_static("dd-api-key");
    pub const APPLICATION_JSON: HeaderValue = HeaderValue::from_static("application/json");
    pub const APPLICATION_MSGPACK: HeaderValue = HeaderValue::from_static(APPLICATION_MSGPACK_STR);
    pub const APPLICATION_PROTOBUF: HeaderValue =
        HeaderValue::from_static(APPLICATION_PROTOBUF_STR);
    pub const X_DATADOG_TEST_SESSION_TOKEN: HeaderName =
        HeaderName::from_static("x-datadog-test-session-token");
}

pub type HttpClient = http_common::GenericHttpClient<connector::Connector>;
pub type GenericHttpClient<C> = http_common::GenericHttpClient<C>;
pub type HttpResponse = http_common::HttpResponse;
pub type HttpRequestBuilder = hyper::http::request::Builder;
pub trait Connect:
    hyper_util::client::legacy::connect::Connect + Clone + Send + Sync + 'static
{
}
impl<C: hyper_util::client::legacy::connect::Connect + Clone + Send + Sync + 'static> Connect
    for C
{
}

// Used by tag! macro
pub use const_format;

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Endpoint {
    #[serde(serialize_with = "serialize_uri", deserialize_with = "deserialize_uri")]
    pub url: hyper::Uri,
    pub api_key: Option<Cow<'static, str>>,
    pub timeout_ms: u64,
    /// Sets X-Datadog-Test-Session-Token header on any request
    pub test_token: Option<Cow<'static, str>>,
}

impl Default for Endpoint {
    fn default() -> Self {
        Endpoint {
            url: hyper::Uri::default(),
            api_key: None,
            timeout_ms: Self::DEFAULT_TIMEOUT,
            test_token: None,
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct SerializedUri<'a> {
    scheme: Option<Cow<'a, str>>,
    authority: Option<Cow<'a, str>>,
    path_and_query: Option<Cow<'a, str>>,
}

fn serialize_uri<S>(uri: &hyper::Uri, serializer: S) -> Result<S::Ok, S::Error>
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

fn deserialize_uri<'de, D>(deserializer: D) -> Result<hyper::Uri, D::Error>
where
    D: Deserializer<'de>,
{
    let uri = SerializedUri::deserialize(deserializer)?;
    let mut builder = hyper::Uri::builder();
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

/// TODO: we should properly handle malformed urls
/// * For windows and unix schemes:
///     * For compatibility reasons with existing implementation this parser stores the encoded path
///       in authority section as there is no existing standard [see](https://github.com/whatwg/url/issues/577)
///       that covers this. We need to pick one hack or another
///     * For windows, interprets everything after windows: as path
///     * For unix, interprets everything after unix:// as path
/// * For file scheme implementation will simply backfill missing authority section
pub fn parse_uri(uri: &str) -> anyhow::Result<hyper::Uri> {
    if let Some(path) = uri.strip_prefix("unix://") {
        encode_uri_path_in_authority("unix", path)
    } else if let Some(path) = uri.strip_prefix("windows:") {
        encode_uri_path_in_authority("windows", path)
    } else if let Some(path) = uri.strip_prefix("file://") {
        encode_uri_path_in_authority("file", path)
    } else {
        Ok(hyper::Uri::from_str(uri)?)
    }
}

fn encode_uri_path_in_authority(scheme: &str, path: &str) -> anyhow::Result<hyper::Uri> {
    let mut parts = uri::Parts::default();
    parts.scheme = uri::Scheme::from_str(scheme).ok();

    let path = hex::encode(path);

    parts.authority = uri::Authority::from_str(path.as_str()).ok();
    parts.path_and_query = Some(uri::PathAndQuery::from_static(""));
    Ok(hyper::Uri::from_parts(parts)?)
}

pub fn decode_uri_path_in_authority(uri: &hyper::Uri) -> anyhow::Result<PathBuf> {
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
        let mut builder = hyper::Request::builder()
            .uri(self.url.clone())
            .header(hyper::header::USER_AGENT, user_agent);

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
    pub fn from_url(url: hyper::Uri) -> Endpoint {
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

    /// Creates a reqwest ClientBuilder configured for this endpoint.
    ///
    /// This method handles various endpoint schemes:
    /// - `http`/`https`: Standard HTTP(S) endpoints
    /// - `unix`: Unix domain sockets (Unix only)
    /// - `windows`: Windows named pipes (Windows only)
    /// - `file`: File dump endpoints for debugging (spawns a local server to capture requests)
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

        let mut builder =
            reqwest::Client::builder().timeout(std::time::Duration::from_millis(self.timeout_ms));

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
