// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{borrow::Cow, ops::Deref, path::PathBuf, str::FromStr};

use hyper::{
    header::HeaderValue,
    http::uri::{self},
};
use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub mod azure_app_services;
pub mod connector;
pub mod entity_id;
#[macro_use]
pub mod cstr;
pub mod config;
pub mod rate_limiter;
pub mod tag;

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

pub type HttpClient = hyper::Client<connector::Connector, hyper::Body>;
pub type HttpResponse = hyper::Response<hyper::Body>;
pub type HttpRequestBuilder = hyper::http::request::Builder;

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
    let path = hex::decode(
        uri.authority()
            .ok_or_else(|| anyhow::anyhow!("missing uri authority"))?
            .as_str(),
    )?;
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

    /// Return a request builder with the following headers:
    /// - User agent
    /// - Api key
    /// - Container Id/Entity Id
    pub fn to_request_builder(&self, user_agent: &str) -> anyhow::Result<HttpRequestBuilder> {
        let mut builder = hyper::Request::builder()
            .uri(self.url.clone())
            .header(hyper::header::USER_AGENT, user_agent);

        // Add the Api key header if available
        if let Some(api_key) = &self.api_key {
            builder = builder.header(header::DATADOG_API_KEY, HeaderValue::from_str(api_key)?);
        }

        // Add the test session token if available
        if let Some(token) = &self.test_token {
            builder = builder.header(
                header::X_DATADOG_TEST_SESSION_TOKEN,
                HeaderValue::from_str(token)?,
            );
        }

        // Add the Container Id header if available
        if let Some(container_id) = entity_id::get_container_id() {
            builder = builder.header(header::DATADOG_CONTAINER_ID, container_id);
        }

        // Add the Entity Id header if available
        if let Some(entity_id) = entity_id::get_entity_id() {
            builder = builder.header(header::DATADOG_ENTITY_ID, entity_id);
        }

        // Add the External Env header if available
        if let Some(external_env) = entity_id::get_external_env() {
            builder = builder.header(header::DATADOG_EXTERNAL_ENV, external_env);
        }

        Ok(builder)
    }

    #[inline]
    pub fn from_slice(url: &str) -> Endpoint {
        Endpoint {
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
}
