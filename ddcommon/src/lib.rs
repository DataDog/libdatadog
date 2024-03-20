// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{borrow::Cow, ops::Deref, str::FromStr};

use hyper::{
    header::HeaderValue,
    http::uri::{self},
};
use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub mod azure_app_services;
pub mod connector;
pub mod container_id;
#[macro_use]
pub mod cstr;
pub mod config;
pub mod tag;

pub mod header {
    #![allow(clippy::declare_interior_mutable_const)]
    use hyper::{header::HeaderName, http::HeaderValue};
    pub const DATADOG_CONTAINER_ID: HeaderName = HeaderName::from_static("datadog-container-id");
    pub const DATADOG_TRACE_COUNT: HeaderName = HeaderName::from_static("x-datadog-trace-count");

    pub const DATADOG_API_KEY: HeaderName = HeaderName::from_static("dd-api-key");
    pub const APPLICATION_JSON: HeaderValue = HeaderValue::from_static("application/json");
    pub const APPLICATION_MSGPACK: HeaderValue = HeaderValue::from_static("application/msgpack");
}

pub type HttpClient = hyper::Client<connector::Connector, hyper::Body>;
pub type HttpResponse = hyper::Response<hyper::Body>;
pub type HttpRequestBuilder = hyper::http::request::Builder;

#[derive(Default, Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Endpoint {
    #[serde(serialize_with = "serialize_uri", deserialize_with = "deserialize_uri")]
    pub url: hyper::Uri,
    pub api_key: Option<Cow<'static, str>>,
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

// TODO: we should properly handle malformed urls
// * For windows and unix schemes:
//     * For compatibility reasons with existing implementation this parser stores the encoded path
//       in authority section as there is no existing standard
//       [see](https://github.com/whatwg/url/issues/577) that covers this. We need to pick one hack
//       or another
// * For file scheme implementation will simply backfill missing authority section
pub fn parse_uri(uri: &str) -> anyhow::Result<hyper::Uri> {
    let scheme_pos = if let Some(scheme_pos) = uri.find("://") {
        scheme_pos
    } else {
        return Ok(hyper::Uri::from_str(uri)?);
    };

    let scheme = &uri[0..scheme_pos];
    let rest = &uri[scheme_pos + 3..];
    match scheme {
        "windows" | "unix" => {
            let mut parts = uri::Parts::default();
            parts.scheme = uri::Scheme::from_str(scheme).ok();

            let path = hex::encode(rest);

            parts.authority = uri::Authority::from_str(path.as_str()).ok();
            parts.path_and_query = Some(uri::PathAndQuery::from_static(""));
            Ok(hyper::Uri::from_parts(parts)?)
        }
        "file" => {
            let mut parts = uri::Parts::default();
            parts.scheme = uri::Scheme::from_str(scheme).ok();
            parts.authority = Some(uri::Authority::from_static("localhost"));

            // TODO: handle edge cases like improperly escaped url strings
            //
            // this is eventually user configurable field
            // anything we can do to ensure invalid input becomes valid - will improve usability
            parts.path_and_query = uri::PathAndQuery::from_str(rest).ok();

            Ok(hyper::Uri::from_parts(parts)?)
        }
        _ => Ok(hyper::Uri::from_str(uri)?),
    }
}

impl Endpoint {
    pub fn into_request_builder(&self, user_agent: &str) -> anyhow::Result<HttpRequestBuilder> {
        let mut builder = hyper::Request::builder()
            .uri(self.url.clone())
            .header(hyper::header::USER_AGENT, user_agent);

        if let Some(api_key) = &self.api_key {
            builder = builder.header(header::DATADOG_API_KEY, HeaderValue::from_str(api_key)?);
        }

        if let Some(container_id) = container_id::get_container_id() {
            builder = builder.header(header::DATADOG_CONTAINER_ID, container_id);
        }

        Ok(builder)
    }
}
