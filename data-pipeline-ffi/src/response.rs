// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Define FFI compatible AgentResponse struct

use data_pipeline::trace_exporter::agent_response::AgentResponse;
use std::{
    ffi::{c_char, CString},
    ptr::null,
};

/// Structure containing the agent response to a trace payload
/// MUST be freed with `ddog_trace_exporter_response_free`
///
/// If the agent payload version is enabled on the trace exporter, and
/// the agent response indicates that the payload version hasn't changed,
/// the body will be empty.
#[derive(Debug, Default)]
pub struct ExporterResponse {
    /// The body of the response, which is a string containing the response from the agent.
    pub body: CString,
    pub body_len: usize,
}

impl From<AgentResponse> for ExporterResponse {
    fn from(value: AgentResponse) -> Self {
        match value {
            AgentResponse::Changed { body } => ExporterResponse {
                body_len: body.len(),
                body: CString::new(body).unwrap_or_default(),
            },
            AgentResponse::Unchanged => ExporterResponse {
                body: CString::new("").unwrap_or_default(),
                body_len: 0,
            },
        }
    }
}

/// Return a read-only pointer to the response body. This pointer is only valid as long as
/// `response` is valid.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_response_get_body(
    response: *const ExporterResponse,
    out_len: Option<&mut usize>,
) -> *const c_char {
    if response.is_null() {
        null()
    } else {
        if let Some(len) = out_len {
            *len = (*response).body_len;
        }
        (*response).body.as_ptr()
    }
}

/// Free `response` and all its contents. After being called response will not point to a valid
/// memory address so any further actions on it could lead to undefined behavior.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_response_free(response: *mut ExporterResponse) {
    if !response.is_null() {
        drop(Box::from_raw(response));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    #[test]
    fn constructor_test_changed() {
        let agent_response = AgentResponse::Changed {
            body: "res".to_string(),
        };
        let response = &ExporterResponse::from(agent_response) as *const ExporterResponse;
        let mut len = 0;
        let body = unsafe {
            CStr::from_ptr(ddog_trace_exporter_response_get_body(
                response,
                Some(&mut len),
            ))
            .to_string_lossy()
        };
        assert_eq!(body, "res".to_string());
        assert_eq!(len, 3);
    }

    #[test]
    fn constructor_test_unchanged() {
        let agent_response = AgentResponse::Unchanged;
        let response = Box::into_raw(Box::new(ExporterResponse::from(agent_response)));
        let mut len = usize::MAX;
        let body = unsafe {
            CStr::from_ptr(ddog_trace_exporter_response_get_body(
                response,
                Some(&mut len),
            ))
            .to_string_lossy()
        };
        assert_eq!(body, "".to_string());
        assert_eq!(len, 0);

        unsafe {
            ddog_trace_exporter_response_free(response);
        }
    }

    #[test]
    fn handle_null_test() {
        unsafe {
            let body = ddog_trace_exporter_response_get_body(null(), None);
            assert!(body.is_null());

            ddog_trace_exporter_response_free(null::<ExporterResponse>() as *mut ExporterResponse);
        }
    }
}
