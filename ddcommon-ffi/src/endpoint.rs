// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::slice::AsBytes;
use crate::Error;
use ddcommon::{parse_uri, Endpoint};
use hyper::http::uri::{Authority, Parts};
use std::str::FromStr;

#[no_mangle]
#[must_use]
pub extern "C" fn ddog_endpoint_from_url(url: crate::CharSlice) -> Option<Box<Endpoint>> {
    parse_uri(url.to_utf8_lossy().as_ref())
        .ok()
        .map(|url| Box::new(Endpoint { url, api_key: None }))
}

// We'll just specify the base site here. If api key provided, different intakes need to use their
// own subdomains.
#[no_mangle]
#[must_use]
pub extern "C" fn ddog_endpoint_from_api_key(api_key: crate::CharSlice) -> Box<Endpoint> {
    let mut parts = Parts::default();
    parts.authority = Some(Authority::from_static("datadoghq.com"));
    Box::new(Endpoint {
        url: hyper::Uri::from_parts(parts).unwrap(),
        api_key: Some(api_key.to_utf8_lossy().to_string().into()),
    })
}

// We'll just specify the base site here. If api key provided, different intakes need to use their
// own subdomains.
#[no_mangle]
#[must_use]
pub extern "C" fn ddog_endpoint_from_api_key_and_site(
    api_key: crate::CharSlice,
    site: crate::CharSlice,
    endpoint: &mut *mut Endpoint,
) -> Option<Box<Error>> {
    let mut parts = Parts::default();
    parts.authority = Some(match Authority::from_str(&site.to_utf8_lossy()) {
        Ok(s) => s,
        Err(e) => return Some(Box::new(Error::from(e.to_string()))),
    });
    *endpoint = Box::into_raw(Box::new(Endpoint {
        url: hyper::Uri::from_parts(parts).unwrap(),
        api_key: Some(api_key.to_utf8_lossy().to_string().into()),
    }));
    None
}

#[no_mangle]
#[must_use]
pub extern "C" fn ddog_endpoint_empty() -> Box<Endpoint> {
    Box::new(Endpoint::default())
}

#[no_mangle]
pub extern "C" fn ddog_endpoint_drop(_: Box<Endpoint>) {}
