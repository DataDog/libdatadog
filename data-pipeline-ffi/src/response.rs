// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Define FFI compatible AgentResponse struct

use data_pipeline::trace_exporter::agent_response::AgentResponse;
use std::ptr::null;

/// Structure containing the agent response to a trace payload
/// MUST be freed with `ddog_trace_exporter_response_free`
///
/// If the agent payload version is enabled on the trace exporter, and
/// the agent response indicates that the payload version hasn't changed,
/// the body will be empty.
#[derive(Debug, Default)]
pub struct ExporterResponse {
    /// The body of the response, which is a string containing the response from the agent.
    pub body: Option<Vec<u8>>,
}

impl From<AgentResponse> for ExporterResponse {
    fn from(value: AgentResponse) -> Self {
        match value {
            AgentResponse::Changed { body } => ExporterResponse {
                body: Some(body.into_bytes()),
            },
            AgentResponse::Unchanged => ExporterResponse { body: None },
        }
    }
}

/// Return a read-only pointer to the response body. This pointer is only valid as long as
/// `response` is valid.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_response_get_body(
    response: *const ExporterResponse,
    out_len: Option<&mut usize>,
) -> *const u8 {
    let mut len: usize = 0;
    let body = if response.is_null() {
        null()
    } else if let Some(body) = &(*response).body {
        len = body.len();
        body.as_ptr()
    } else {
        null()
    };

    if let Some(out_len) = out_len {
        *out_len = len;
    }
    body
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

    #[test]
    fn constructor_test_changed() {
        let agent_response = AgentResponse::Changed {
            body: "res".to_string(),
        };
        let response = &ExporterResponse::from(agent_response) as *const ExporterResponse;
        let mut len = 0;
        let body = unsafe { ddog_trace_exporter_response_get_body(response, Some(&mut len)) };
        let response =
            unsafe { std::str::from_utf8(std::slice::from_raw_parts(body, len)).unwrap() };
        assert_eq!(response, "res");
        assert_eq!(len, 3);
    }

    #[test]
    fn constructor_test_unchanged() {
        let agent_response = AgentResponse::Unchanged;
        let response = Box::into_raw(Box::new(ExporterResponse::from(agent_response)));
        let mut len = usize::MAX;
        let body = unsafe { ddog_trace_exporter_response_get_body(response, Some(&mut len)) };
        assert!(body.is_null());
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
