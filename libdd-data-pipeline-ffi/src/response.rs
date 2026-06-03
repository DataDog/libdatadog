// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Define FFI compatible AgentResponse struct

use libdd_common_ffi::slice::ByteSlice;
use libdd_data_pipeline::trace_exporter::agent_response::AgentResponse;

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

/// Return a borrowed view of the response body.  The returned slice is
/// only valid as long as `response` is alive.  Returns an empty slice
/// when `response` is null or the body is absent.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_response_get_body<'a>(
    response: Option<&'a ExporterResponse>,
) -> ByteSlice<'a> {
    response
        .and_then(|r| Some(ByteSlice::from(r.body.as_deref()?)))
        .unwrap_or_default()
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
    fn body_from_changed_response() {
        let agent_response = AgentResponse::Changed {
            body: "res".to_string(),
        };
        let response = ExporterResponse::from(agent_response);
        let body = unsafe { ddog_trace_exporter_response_get_body(Some(&response)) };
        assert_eq!(body.len(), 3);
        assert_eq!(std::str::from_utf8(&body).unwrap(), "res");
    }

    #[test]
    fn body_from_unchanged_response() {
        let agent_response = AgentResponse::Unchanged;
        let response = ExporterResponse::from(agent_response);
        let body = unsafe { ddog_trace_exporter_response_get_body(Some(&response)) };
        assert_eq!(body.len(), 0);
    }

    #[test]
    fn body_from_null_response() {
        let body = unsafe { ddog_trace_exporter_response_get_body(None) };
        assert_eq!(body.len(), 0);
    }
}
