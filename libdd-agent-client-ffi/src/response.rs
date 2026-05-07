// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C ABI for [`libdd_agent_client::AgentResponse`].
//!
//! `AgentResponse` is produced by
//! [`crate::ddog_agent_client_send_traces_blocking`] and exposes the
//! HTTP status plus a parsed `rate_by_service` sampling-rate map. We
//! serialise the map back to a JSON string at the FFI boundary so
//! callers can parse it with whatever JSON library they prefer.

use libdd_agent_client::AgentResponse;
use libdd_common_ffi::string::StringWrapper;

/// Read the HTTP status code from an [`AgentResponse`].
///
/// Returns 0 if `response` is null.
///
/// # Safety
/// `response` must be `None` or a valid reference produced by this
/// crate.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_response_status(
    response: Option<&AgentResponse>,
) -> u16 {
    response.map(|r| r.status).unwrap_or(0)
}

/// Serialise the parsed `rate_by_service` sampling-rate map to a JSON
/// string.
///
/// Returns `None` (null pointer in C) if `response` is null, the
/// agent did not return a `rate_by_service` field, or serialisation
/// fails. The caller owns the returned `Box<StringWrapper>` and must
/// release it via `ddog_StringWrapper_drop`.
///
/// # Safety
/// `response` must be `None` or a valid reference produced by this
/// crate.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_response_rate_by_service_json(
    response: Option<&AgentResponse>,
) -> Option<Box<StringWrapper>> {
    let response = response?;
    let rates = response.rate_by_service.as_ref()?;
    let json = serde_json::to_string(rates).ok()?;
    Some(Box::new(StringWrapper::from(json)))
}

/// Drop an [`AgentResponse`].
///
/// # Safety
/// `response` must be `None` or a response produced by this crate and
/// not yet dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_response_drop(response: Option<Box<AgentResponse>>) {
    drop(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_response() -> AgentResponse {
        let mut rates = HashMap::new();
        rates.insert("service:env".to_owned(), 0.5);
        AgentResponse {
            status: 200,
            rate_by_service: Some(rates),
        }
    }

    #[test]
    fn status_round_trip() {
        let resp = make_response();
        unsafe {
            assert_eq!(ddog_agent_response_status(Some(&resp)), 200);
            assert_eq!(ddog_agent_response_status(None), 0);
        }
    }

    #[test]
    fn rate_by_service_json_round_trip() {
        let resp = make_response();
        unsafe {
            let s = ddog_agent_response_rate_by_service_json(Some(&resp));
            assert!(s.is_some());
            let json = s.unwrap();
            let json_str: &str = (*json).as_ref();
            let parsed: HashMap<String, f64> = serde_json::from_str(json_str).unwrap();
            assert_eq!(parsed.get("service:env"), Some(&0.5));
        }
    }

    #[test]
    fn rate_by_service_json_absent() {
        let resp = AgentResponse {
            status: 200,
            rate_by_service: None,
        };
        unsafe {
            let s = ddog_agent_response_rate_by_service_json(Some(&resp));
            assert!(s.is_none());
        }
    }

    #[test]
    fn drop_handles_none() {
        unsafe { ddog_agent_response_drop(None) };
    }
}
