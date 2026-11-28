// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use alloc::boxed::Box;

#[cfg(all(feature = "catch_panic", panic = "unwind"))]
use libdd_common_ffi::wrap_with_ffi_result;
#[cfg(not(all(feature = "catch_panic", panic = "unwind")))]
use libdd_common_ffi::wrap_with_ffi_result_no_catch as wrap_with_ffi_result;
use libdd_common_ffi::{slice::ByteSlice, string::StringWrapperResult};
use libdd_config_core::WireResolver;

#[derive(Default)]
pub struct CoreConfigResolver(WireResolver);

/// Allocate a startup configuration resolver. The caller owns the returned handle.
// allow(ffi-panic-boundary): Only allocates a resolver with empty schema and pending fields.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_config_core_new() -> Box<CoreConfigResolver> {
    Box::default()
}

/// Send a version 1 configuration request. The result owns its JSON or FFI error.
///
/// # Safety
///
/// A non-null resolver must point to a live handle with exclusive access for this call.
/// The request must refer to readable memory for its full length and cannot alias the resolver.
#[unsafe(no_mangle)]
#[function_name::named]
pub unsafe extern "C" fn ddog_config_core_request(
    resolver: Option<&mut CoreConfigResolver>,
    request: ByteSlice<'_>,
) -> StringWrapperResult {
    wrap_with_ffi_result!({
        let resolver = resolver.ok_or_else(|| anyhow::anyhow!("null configuration resolver"))?;
        let request = request.try_as_slice()?;
        anyhow::Ok(resolver.0.request_text(request)?)
    })
}

/// Release the resolver. Null is allowed.
///
/// # Safety
///
/// A non-null handle must have come from `ddog_config_core_new` and must have no other users.
// allow(ffi-panic-boundary): Drops owned configuration data without calling a host transform.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_config_core_drop(_resolver: Option<Box<CoreConfigResolver>>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_wire_request_and_reuses_the_schema() {
        let mut resolver = ddog_config_core_new();
        let initialize = br#"{"version":1,"operation":"initialize","manifest":{"kind":"json",
            "value":"{\"version\":\"2\",\"supportedConfigurations\":{\"DD_MODE\":[{\"type\":\"string\",\"default\":\"info\"}]}}"}}"#;
        // SAFETY: The exclusive handle and request are live until the call returns.
        let reply = unsafe {
            ddog_config_core_request(Some(&mut resolver), ByteSlice::from(initialize.as_slice()))
        }
        .unwrap();
        assert!(reply.as_ref().contains(r#""origin":"default""#));
        assert!(reply.as_ref().contains(r#""value":"info""#));

        let resolve = br#"{"version":1,"operation":"resolve","inputs":{"layers":[
            {"origin":"env_var","entries":[{"name":"DD_MODE","value":"debug"}]}]}}"#;
        // SAFETY: The exclusive handle and request are live until the call returns.
        let reply = unsafe {
            ddog_config_core_request(Some(&mut resolver), ByteSlice::from(resolve.as_slice()))
        }
        .unwrap();
        assert!(reply.as_ref().contains(r#""origin":"env_var""#));
        assert!(reply.as_ref().contains(r#""value":"debug""#));
        // SAFETY: This transfers the owned handle and no caller uses it afterwards.
        unsafe { ddog_config_core_drop(Some(resolver)) };
    }

    #[test]
    fn accepts_null_for_release_and_reports_null_for_requests() {
        // SAFETY: Both functions accept a null handle and the request is empty.
        unsafe {
            assert!(matches!(
                ddog_config_core_request(None, ByteSlice::default()),
                StringWrapperResult::Err(_)
            ));
            ddog_config_core_drop(None);
        }
    }
}
