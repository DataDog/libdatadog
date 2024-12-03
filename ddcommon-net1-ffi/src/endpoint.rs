// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon_ffi::slice::AsBytes;
use ddcommon_ffi::Error;
use ddcommon_net1::{dep::http, parse_uri};
use std::borrow::Cow;
use std::str::FromStr;

use http::uri::{Authority, Parts};

// Bindgen doesn't understand modules, this is the same type as far as bindgen
// is concerned. Using a transparent repr is important here.
mod bindgen {
    // Create a wrapper struct for FFI, since bindgen doesn't forward declare
    // the struct without it.
    #[allow(dead_code)]
    #[repr(transparent)]
    #[derive(Clone, PartialEq, Eq, Hash, Debug)]
    pub struct Endpoint(ddcommon_net1::Endpoint);
}

pub type Endpoint = ddcommon_net1::Endpoint;

#[no_mangle]
#[must_use]
pub extern "C" fn ddog_endpoint_from_url(url: ddcommon_ffi::CharSlice) -> Option<Box<Endpoint>> {
    parse_uri(url.to_utf8_lossy().as_ref())
        .ok()
        .map(|url| Box::new(Endpoint::from_url(url)))
}

#[no_mangle]
#[must_use]
pub extern "C" fn ddog_endpoint_from_filename(
    filename: ddcommon_ffi::CharSlice,
) -> Option<Box<Endpoint>> {
    let url = format!("file://{}", filename.to_utf8_lossy());
    Some(Box::new(Endpoint::from_slice(&url)))
}

// We'll just specify the base site here. If api key provided, different intakes need to use their
// own subdomains.
#[no_mangle]
#[must_use]
pub extern "C" fn ddog_endpoint_from_api_key(api_key: ddcommon_ffi::CharSlice) -> Box<Endpoint> {
    let mut parts = Parts::default();
    parts.authority = Some(Authority::from_static("datadoghq.com"));
    Box::new(Endpoint {
        url: http::Uri::from_parts(parts).unwrap(),
        api_key: Some(api_key.to_utf8_lossy().to_string().into()),
        ..Default::default()
    })
}

// We'll just specify the base site here. If api key provided, different intakes need to use their
// own subdomains.
#[no_mangle]
#[must_use]
pub extern "C" fn ddog_endpoint_from_api_key_and_site(
    api_key: ddcommon_ffi::CharSlice,
    site: ddcommon_ffi::CharSlice,
    endpoint: &mut *mut Endpoint,
) -> Option<Box<Error>> {
    let mut parts = Parts::default();
    parts.authority = Some(match Authority::from_str(&site.to_utf8_lossy()) {
        Ok(s) => s,
        Err(e) => return Some(Box::new(Error::from(e.to_string()))),
    });
    *endpoint = Box::into_raw(Box::new(Endpoint {
        url: http::Uri::from_parts(parts).unwrap(),
        api_key: Some(api_key.to_utf8_lossy().to_string().into()),
        ..Default::default()
    }));
    None
}

#[no_mangle]
extern "C" fn ddog_endpoint_set_timeout(endpoint: &mut Endpoint, millis: u64) {
    endpoint.timeout_ms = millis;
}

#[no_mangle]
extern "C" fn ddog_endpoint_set_test_token(
    endpoint: &mut Endpoint,
    token: ddcommon_ffi::CharSlice,
) {
    endpoint.test_token = if token.is_empty() {
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
    use ddcommon_ffi::CharSlice;

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
        assert_eq!(
            endpoint.as_ref().unwrap().timeout_ms,
            Endpoint::DEFAULT_TIMEOUT
        );

        ddog_endpoint_set_timeout(endpoint.as_mut().unwrap(), 2000);
        assert_eq!(endpoint.unwrap().timeout_ms, 2000);

        let mut endpoint_api_key = ddog_endpoint_from_api_key(CharSlice::from("test-key"));
        assert_eq!(endpoint_api_key.timeout_ms, Endpoint::DEFAULT_TIMEOUT);

        ddog_endpoint_set_timeout(&mut endpoint_api_key, 2000);
        assert_eq!(endpoint_api_key.timeout_ms, 2000);
    }
}
