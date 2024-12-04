// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::slice::AsBytes;
use crate::Error;
use hyper::http::uri::{Authority, Parts};
use std::borrow::Cow;
use std::str::FromStr;

use ddcommon_net1 as net;
use net::parse_uri;

/// Wrapper type to generate the correct FFI name.
pub struct Endpoint(net::Endpoint);

impl From<net::Endpoint> for Endpoint {
    fn from(inner: net::Endpoint) -> Self {
        Self(inner)
    }
}

impl From<Endpoint> for net::Endpoint {
    fn from(inner: Endpoint) -> Self {
        inner.0
    }
}

#[no_mangle]
#[must_use]
pub extern "C" fn ddog_endpoint_from_url(url: crate::CharSlice) -> Option<Box<Endpoint>> {
    parse_uri(url.to_utf8_lossy().as_ref())
        .ok()
        .map(|url| Box::new(net::Endpoint::from_url(url).into()))
}

#[no_mangle]
#[must_use]
pub extern "C" fn ddog_endpoint_from_filename(filename: crate::CharSlice) -> Option<Box<Endpoint>> {
    let url = format!("file://{}", filename.to_utf8_lossy());
    Some(Box::new(net::Endpoint::from_slice(&url).into()))
}

// We'll just specify the base site here. If api key provided, different intakes need to use their
// own subdomains.
#[no_mangle]
#[must_use]
pub extern "C" fn ddog_endpoint_from_api_key(api_key: crate::CharSlice) -> Box<Endpoint> {
    let mut parts = Parts::default();
    parts.authority = Some(Authority::from_static("datadoghq.com"));
    Box::new(
        net::Endpoint {
            url: hyper::Uri::from_parts(parts).unwrap(),
            api_key: Some(api_key.to_utf8_lossy().to_string().into()),
            ..Default::default()
        }
        .into(),
    )
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
    *endpoint = Box::into_raw(Box::new(
        net::Endpoint {
            url: hyper::Uri::from_parts(parts).unwrap(),
            api_key: Some(api_key.to_utf8_lossy().to_string().into()),
            ..Default::default()
        }
        .into(),
    ));
    None
}

#[no_mangle]
extern "C" fn ddog_endpoint_set_timeout(endpoint: &mut Endpoint, millis: u64) {
    endpoint.0.timeout_ms = millis;
}

#[no_mangle]
extern "C" fn ddog_endpoint_set_test_token(endpoint: &mut Endpoint, token: crate::CharSlice) {
    endpoint.0.test_token = if token.is_empty() {
        None
    } else {
        Some(Cow::Owned(token.to_utf8_lossy().to_string()))
    };
}

#[no_mangle]
pub extern "C" fn ddog_endpoint_drop(_: Box<Endpoint>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CharSlice;
    use net::Endpoint;

    #[test]
    fn test_ddog_endpoint_from_url() {
        let cases = [
            ("", false),
            ("http:// /hey", false),
            ("file://", false),
            ("http://localhost:8383/hello", true),
            ("file:/// file / with/weird chars 🤡", true),
            ("file://./", true),
            ("unix://./", true),
        ];

        for (input, expected) in cases {
            let actual = ddog_endpoint_from_url(CharSlice::from(input)).is_some();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn set_timeout() {
        let url = CharSlice::from("http://127.0.0.1");

        let mut endpoint = ddog_endpoint_from_url(url);
        let timeout_ms = endpoint.as_ref().unwrap().0.timeout_ms;
        assert_eq!(timeout_ms, Endpoint::DEFAULT_TIMEOUT);

        ddog_endpoint_set_timeout(endpoint.as_mut().unwrap(), 2000);
        let timeout_ms = endpoint.as_ref().unwrap().0.timeout_ms;
        assert_eq!(timeout_ms, 2000);

        let mut endpoint_api_key = ddog_endpoint_from_api_key(CharSlice::from("test-key"));
        let timeout_ms = endpoint_api_key.0.timeout_ms;
        assert_eq!(timeout_ms, Endpoint::DEFAULT_TIMEOUT);

        ddog_endpoint_set_timeout(&mut endpoint_api_key, 2000);
        let timeout_ms = endpoint_api_key.0.timeout_ms;
        assert_eq!(timeout_ms, 2000);
    }
}
