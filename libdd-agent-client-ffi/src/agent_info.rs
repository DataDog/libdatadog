// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C ABI for [`libdd_agent_client::AgentInfo`].
//!
//! The Rust `AgentInfo` carries a `serde_json::Value` for the `config`
//! field. We deliberately do **not** expose that type across the FFI
//! boundary; instead, [`ddog_agent_info_config_json`] serialises it to
//! a JSON string at the boundary so callers can parse it with whatever
//! JSON library they prefer.
//!
//! `version`, `container_tags_hash`, and `state_hash` are
//! `Option<String>` on the Rust side. They surface as
//! `ddog_StringWrapper` getters that return `null` when absent — a
//! `Some(...)` Rust value materialises an owned `StringWrapper` that
//! the caller releases via `ddog_StringWrapper_drop`.

use crate::error::DdogAgentClientError;
use crate::string_slice::DdogStringSlice;
use libdd_agent_client::AgentInfo;
use libdd_common_ffi::string::StringWrapper;

/// Drop an [`AgentInfo`] handle.
///
/// # Safety
/// `info` must be `None` or an info handle produced by
/// [`crate::ddog_agent_client_agent_info_blocking`] and not yet
/// dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_info_drop(info: Option<Box<AgentInfo>>) {
    drop(info)
}

/// Borrow the agent's reported endpoints (e.g. `["/v0.4/traces",
/// "/v0.5/traces"]`).
///
/// The returned `DdogStringSlice` borrows from `info`; the entries
/// remain valid while `info` is alive. The outer array is owned by
/// the caller and must be released via
/// [`crate::ddog_string_slice_drop`].
///
/// If `info` is `None`, returns an empty slice.
///
/// # Safety
/// `info` must be `None` or a valid reference to an `AgentInfo`
/// produced by this crate.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_info_endpoints(
    info: Option<&AgentInfo>,
) -> DdogStringSlice<'_> {
    match info {
        Some(i) => DdogStringSlice::from_strings(&i.endpoints),
        None => DdogStringSlice::empty(),
    }
}

/// Whether the agent supports client-side P0 dropping.
///
/// Returns `false` if `info` is null.
///
/// # Safety
/// `info` must be `None` or a valid reference produced by this crate.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_info_client_drop_p0s(info: Option<&AgentInfo>) -> bool {
    info.map(|i| i.client_drop_p0s).unwrap_or(false)
}

/// Borrow the agent's version string, if reported.
///
/// Returns `None` (null pointer in C) when `info` is null or the
/// agent did not report a version. On `Some`, the caller owns the
/// returned `Box<StringWrapper>` and must release it via
/// `ddog_StringWrapper_drop`.
///
/// # Safety
/// `info` must be `None` or a valid reference produced by this crate.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_info_version(
    info: Option<&AgentInfo>,
) -> Option<Box<StringWrapper>> {
    info.and_then(|i| i.version.as_deref())
        .map(|s| Box::new(StringWrapper::from(s)))
}

/// Value of the `Datadog-Container-Tags-Hash` response header from the
/// last `/info` fetch, if any.
///
/// # Safety
/// `info` must be `None` or a valid reference produced by this crate.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_info_container_tags_hash(
    info: Option<&AgentInfo>,
) -> Option<Box<StringWrapper>> {
    info.and_then(|i| i.container_tags_hash.as_deref())
        .map(|s| Box::new(StringWrapper::from(s)))
}

/// Value of the `Datadog-Agent-State` response header from the last
/// `/info` fetch.
///
/// The agent updates this opaque token whenever its internal state
/// changes (e.g. a configuration reload). Clients that poll `/info`
/// periodically can skip re-parsing the response body by comparing
/// this value across calls.
///
/// # Safety
/// `info` must be `None` or a valid reference produced by this crate.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_info_state_hash(
    info: Option<&AgentInfo>,
) -> Option<Box<StringWrapper>> {
    info.and_then(|i| i.state_hash.as_deref())
        .map(|s| Box::new(StringWrapper::from(s)))
}

/// Serialise the agent-reported `config` JSON block to a JSON string.
///
/// Returns the canonical JSON (no whitespace) representation of the
/// `config` value. The Rust API exposes a `serde_json::Value`; we
/// serialise it at the FFI boundary so callers can parse it with
/// whatever JSON library they prefer.
///
/// Returns `null` if `info` is null or if serialisation fails (which
/// is essentially impossible for a `Value` produced by a successful
/// deserialisation, but we return null defensively rather than
/// panicking). The caller owns the returned `Box<StringWrapper>` and
/// must release it via `ddog_StringWrapper_drop`.
///
/// # Safety
/// `info` must be `None` or a valid reference produced by this crate.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_info_config_json(
    info: Option<&AgentInfo>,
) -> Option<Box<StringWrapper>> {
    let info = info?;
    let json = serde_json::to_string(&info.config).ok()?;
    Some(Box::new(StringWrapper::from(json)))
}

/// Same as [`ddog_agent_info_config_json`] but writes the JSON into a
/// caller-managed `*mut Box<StringWrapper>` and returns a flat error
/// for failure. Provided for callers that prefer the explicit-error
/// style used by the rest of the API.
///
/// # Safety
/// `info` must be `None` or a valid reference; `out` must be a valid
/// writable pointer to an uninitialised `*mut ddog_StringWrapper`.
#[no_mangle]
pub unsafe extern "C" fn ddog_agent_info_config_json_or_error(
    info: Option<&AgentInfo>,
    out: Option<&mut *mut StringWrapper>,
) -> Option<Box<DdogAgentClientError>> {
    let Some(info) = info else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "info is null",
        )));
    };
    let Some(out) = out else {
        return Some(Box::new(DdogAgentClientError::invalid_argument(
            "out is null",
        )));
    };
    match serde_json::to_string(&info.config) {
        Ok(s) => {
            *out = Box::into_raw(Box::new(StringWrapper::from(s)));
            None
        }
        Err(e) => Some(Box::new(DdogAgentClientError::new(
            crate::DdogAgentClientErrorCode::Encoding,
            &format!("failed to serialise config JSON: {e}"),
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_info() -> AgentInfo {
        AgentInfo {
            endpoints: vec!["/v0.4/traces".to_owned(), "/v0.5/traces".to_owned()],
            client_drop_p0s: true,
            config: json!({"hostname": "host-a", "default_env": "none"}),
            version: Some("7.50.0".to_owned()),
            container_tags_hash: Some("abc123".to_owned()),
            state_hash: Some("state-001".to_owned()),
        }
    }

    fn empty_info() -> AgentInfo {
        AgentInfo {
            endpoints: Vec::new(),
            client_drop_p0s: false,
            config: serde_json::Value::Null,
            version: None,
            container_tags_hash: None,
            state_hash: None,
        }
    }

    #[test]
    fn endpoints_round_trip() {
        let info = make_info();
        unsafe {
            let slice = ddog_agent_info_endpoints(Some(&info));
            assert_eq!(slice.len, 2);
            crate::ddog_string_slice_drop(slice);
        }
    }

    #[test]
    fn endpoints_null_returns_empty() {
        unsafe {
            let slice = ddog_agent_info_endpoints(None);
            assert_eq!(slice.len, 0);
            crate::ddog_string_slice_drop(slice);
        }
    }

    #[test]
    fn client_drop_p0s_round_trip() {
        let info = make_info();
        unsafe {
            assert!(ddog_agent_info_client_drop_p0s(Some(&info)));
            assert!(!ddog_agent_info_client_drop_p0s(None));
        }
    }

    #[test]
    fn version_present_returns_string() {
        let info = make_info();
        unsafe {
            let s = ddog_agent_info_version(Some(&info));
            assert!(s.is_some());
            let s = s.unwrap();
            let s_str: &str = (*s).as_ref();
            assert_eq!(s_str, "7.50.0");
            drop(s);
        }
    }

    #[test]
    fn version_absent_returns_none() {
        let info = empty_info();
        unsafe {
            let s = ddog_agent_info_version(Some(&info));
            assert!(s.is_none());
        }
    }

    #[test]
    fn container_tags_hash_round_trip() {
        let info = make_info();
        unsafe {
            let s = ddog_agent_info_container_tags_hash(Some(&info));
            assert!(s.is_some());
            let unwrapped = s.unwrap();
            let s_str: &str = (*unwrapped).as_ref();
            assert_eq!(s_str, "abc123");
        }
    }

    #[test]
    fn state_hash_round_trip() {
        let info = make_info();
        unsafe {
            let s = ddog_agent_info_state_hash(Some(&info));
            assert!(s.is_some());
            let unwrapped = s.unwrap();
            let s_str: &str = (*unwrapped).as_ref();
            assert_eq!(s_str, "state-001");
        }
    }

    #[test]
    fn config_json_serialises() {
        let info = make_info();
        unsafe {
            let s = ddog_agent_info_config_json(Some(&info));
            assert!(s.is_some());
            let json = s.unwrap();
            // Confirm it's parseable JSON and the right keys are
            // present. We don't pin exact ordering since serde_json
            // may shuffle.
            let json_str: &str = (*json).as_ref();
            let parsed: serde_json::Value = serde_json::from_str(json_str).unwrap();
            assert_eq!(parsed["hostname"], "host-a");
            assert_eq!(parsed["default_env"], "none");
        }
    }

    #[test]
    fn config_json_or_error_round_trip() {
        let info = make_info();
        unsafe {
            let mut out: *mut StringWrapper = std::ptr::null_mut();
            let err = ddog_agent_info_config_json_or_error(Some(&info), Some(&mut out));
            assert!(err.is_none());
            assert!(!out.is_null());
            // Reclaim the StringWrapper to avoid a leak.
            drop(Box::from_raw(out));
        }
    }

    #[test]
    fn config_json_or_error_null_info() {
        unsafe {
            let mut out: *mut StringWrapper = std::ptr::null_mut();
            let err = ddog_agent_info_config_json_or_error(None, Some(&mut out));
            assert!(err.is_some());
        }
    }

    #[test]
    fn drop_handles_none() {
        unsafe { ddog_agent_info_drop(None) };
    }
}
