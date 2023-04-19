// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::{borrow::Cow, str::FromStr};

use hyper::{
    header::HeaderValue,
    http::uri::{self},
};

pub mod azure_app_services;
pub mod connector;
pub mod container_id;
#[macro_use]
pub mod cstr;
pub mod tag;

pub mod header {
    pub const DATADOG_CONTAINER_ID: &str = "Datadog-Container-ID";
    pub const DATADOG_API_KEY: &str = "DD-API-KEY";
}

pub type HttpClient = hyper::Client<connector::Connector, hyper::Body>;
pub type HttpResponse = hyper::Response<hyper::Body>;
pub type HttpRequestBuilder = hyper::http::request::Builder;

#[derive(Default, Clone, Debug)]
pub struct Endpoint {
    pub url: hyper::Uri,
    pub api_key: Option<Cow<'static, str>>,
}

// TODO: we should properly handle malformed urls
// for windows and unix schemes:
//    for compatibility reasons with existing implementation this parser stores the encoded path in authority section
//    as there is no existing standard https://github.com/whatwg/url/issues/577 that covers this. We need to pick one hack or another
// for file scheme implementation will simply backfill missing authority section
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
