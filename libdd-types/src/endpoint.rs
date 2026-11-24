// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use alloc::borrow::Cow;
use anyhow::Context;
use core::{ops::Deref, str::FromStr};
use http::uri::{self, PathAndQuery, Uri};
use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::path::PathBuf;

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

impl core::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Endpoint")
            .field("url", &self.url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("timeout_ms", &self.timeout_ms)
            .field("test_token", &self.test_token)
            .field("use_system_resolver", &self.use_system_resolver)
            .finish()
    }
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

    /// The URL this endpoint points at.
    #[inline]
    pub fn url(&self) -> &http::Uri {
        &self.url
    }

    /// The configured Datadog API key, if any.
    #[inline]
    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    /// The request timeout in milliseconds.
    #[inline]
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }

    /// The test session token, if any. Sets X-Datadog-Test-Session-Token on any request.
    #[inline]
    pub fn test_token(&self) -> Option<&str> {
        self.test_token.as_deref()
    }

    /// Whether the system DNS resolver is used when building the HTTP client.
    #[inline]
    pub fn use_system_resolver(&self) -> bool {
        self.use_system_resolver
    }

    /// Set the URL this endpoint points at.
    pub fn with_url(mut self, url: http::Uri) -> Self {
        self.url = url;
        self
    }

    /// Set the Datadog API key.
    pub fn with_api_key(mut self, api_key: impl Into<Cow<'static, str>>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    /// Set the test session token. Sets X-Datadog-Test-Session-Token on any request.
    pub fn with_test_token(mut self, test_token: impl Into<Cow<'static, str>>) -> Self {
        self.test_token = Some(test_token.into());
        self
    }

    /// Set the URL in place.
    #[inline]
    pub fn set_url(&mut self, url: http::Uri) {
        self.url = url;
    }

    /// Set or clear the API key in place.
    #[inline]
    pub fn set_api_key(&mut self, api_key: Option<Cow<'static, str>>) {
        self.api_key = api_key;
    }

    /// Set the timeout in place. Unlike [`Endpoint::with_timeout`], this is a raw write:
    /// 0 does not fall back to the default timeout.
    #[inline]
    pub fn set_timeout_ms(&mut self, timeout_ms: u64) {
        self.timeout_ms = timeout_ms;
    }

    /// Set or clear the test session token in place.
    #[inline]
    pub fn set_test_token(&mut self, test_token: Option<Cow<'static, str>>) {
        self.test_token = test_token;
    }

    /// Set whether the system DNS resolver is used, in place.
    #[inline]
    pub fn set_use_system_resolver(&mut self, use_system_resolver: bool) {
        self.use_system_resolver = use_system_resolver;
    }
}

#[cfg(test)]
mod tests {
    use super::{Endpoint, parse_uri};

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

    #[test]
    fn builders_set_their_fields() {
        let url = parse_uri("http://localhost:8126").unwrap();
        let endpoint = Endpoint::from_url(url.clone())
            .with_api_key("key")
            .with_test_token(alloc::string::String::from("token"))
            .with_timeout(1234)
            .with_system_resolver(true);
        assert_eq!(endpoint.url(), &url);
        assert_eq!(endpoint.api_key(), Some("key"));
        assert_eq!(endpoint.test_token(), Some("token"));
        assert_eq!(endpoint.timeout_ms(), 1234);
        assert!(endpoint.use_system_resolver());
    }

    #[test]
    fn with_url_replaces_the_url() {
        let url = parse_uri("http://localhost:8126").unwrap();
        let other = parse_uri("http://otherhost:8126").unwrap();
        let endpoint = Endpoint::from_url(url.clone()).with_url(other.clone());
        assert_eq!(endpoint.url(), &other);
    }

    #[test]
    fn setters_mutate_in_place() {
        let url = parse_uri("http://localhost:8126").unwrap();
        let mut endpoint = Endpoint::default();
        endpoint.set_url(url.clone());
        endpoint.set_api_key(Some("key".into()));
        endpoint.set_timeout_ms(0);
        endpoint.set_test_token(None);
        endpoint.set_use_system_resolver(true);

        assert_eq!(endpoint.url(), &url);
        assert_eq!(endpoint.api_key(), Some("key"));
        // set_timeout_ms is a raw write: unlike with_timeout, 0 does not fall
        // back to the default timeout.
        assert_eq!(endpoint.timeout_ms(), 0);
        assert_eq!(endpoint.test_token(), None);
        assert!(endpoint.use_system_resolver());
    }

    #[test]
    fn setters_clear_optional_fields() {
        let mut endpoint = Endpoint::default()
            .with_api_key("key")
            .with_test_token("token");
        endpoint.set_api_key(None);
        endpoint.set_test_token(None);
        assert_eq!(endpoint.api_key(), None);
        assert_eq!(endpoint.test_token(), None);
    }

    #[test]
    fn with_timeout_zero_falls_back_to_default() {
        let endpoint = Endpoint::default().with_timeout(0);
        assert_eq!(endpoint.timeout_ms(), Endpoint::DEFAULT_TIMEOUT);
    }
}
