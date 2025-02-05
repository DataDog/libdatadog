// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Define FFI compatible AgentResponse struct

use data_pipeline::trace_exporter::agent_response::AgentResponse;
use std::ffi::{c_char, CString};

/// Structure containing the agent response to a trace payload
/// MUST be freed with `ddog_trace_exporter_response_free`
#[derive(Debug, Default)]
pub struct ExporterResponse {
    /// This field should only contain a pointer originated from `Cstring::into_raw`
    pub body: CString,
}

impl From<AgentResponse> for ExporterResponse {
    fn from(value: AgentResponse) -> Self {
        ExporterResponse {
            body: CString::new(value.body).unwrap_or_default(),
        }
    }
}

/// Return a read-only pointer to the response body. This pointer is only valid as long as
/// `response` is valid.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_response_get_body(
    response: &ExporterResponse,
) -> *const c_char {
    response.body.as_ptr()
}

/// Free `response` and all its contents. After being called response will not point to a valid
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
        let body = unsafe {
            CStr::from_ptr(ddog_trace_exporter_response_get_body(&response)).to_string_lossy()
        };
        assert_eq!(body, "res".to_string());
    }
}
