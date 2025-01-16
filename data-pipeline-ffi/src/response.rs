// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Define FFI compatible AgentResponse struct

use data_pipeline::trace_exporter::agent_response::AgentResponse;
use std::ffi::{c_char, CString};

/// Structure containing the agent response to a trace payload
/// MUST be freed with `ddog_trace_exporter_response_free`
#[repr(C)]
#[derive(Debug)]
pub struct ExporterResponse {
    pub body: *mut c_char,
}

impl From<AgentResponse> for ExporterResponse {
    fn from(value: AgentResponse) -> Self {
        ExporterResponse {
            body: CString::new(value.body).unwrap_or_default().into_raw(),
        }
    }
}

impl Drop for ExporterResponse {
    fn drop(&mut self) {
        if !self.body.is_null() {
            // SAFETY: `the caller must ensure that `SendResponse` has been created through its
            // `new` method which ensures that `body` property is originated from
            // `Cstring::into_raw` call. Any other posibility could lead to UB.
            unsafe {
                drop(CString::from_raw(self.body));
                self.body = std::ptr::null_mut();
            }
        }
    }
}

/// Frees `response` and all its contents. After being called response will not point to a valid
/// memory address so any further actions on it could lead to undefined behavior.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_response_free(
    response: Option<Box<ExporterResponse>>,
) {
    drop(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    #[test]
    fn constructor_test() {
        let agent_response = AgentResponse {
            body: "res".to_string(),
        };
        let response = Box::new(ExporterResponse::from(agent_response));
        let body = unsafe { CStr::from_ptr(response.body).to_string_lossy() };
        assert_eq!(body, "res".to_string());
    }

    #[test]
    fn destructor_test() {
        let agent_response = AgentResponse {
            body: "res".to_string(),
        };
        let response = Box::new(ExporterResponse::from(agent_response));
        let body = unsafe { CStr::from_ptr(response.body).to_string_lossy() };
        assert_eq!(body, "res".to_string());

        unsafe { ddog_trace_exporter_response_free(Some(response)) };
    }
}
