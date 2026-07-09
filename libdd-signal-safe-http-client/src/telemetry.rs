// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use crate::{BuildError, Header, HttpSink, Method, Request, SendError};

/// Agent telemetry proxy path for APM telemetry payloads.
pub const AGENT_TELEMETRY_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";
/// Direct intake telemetry path for APM telemetry payloads.
pub const DIRECT_TELEMETRY_PATH: &str = "/api/v2/apmtelemetry";
/// JSON content type emitted for telemetry payloads.
pub const APPLICATION_JSON: &str = "application/json";
/// Telemetry request-type header name.
pub const HEADER_REQUEST_TYPE: &str = "DD-Telemetry-Request-Type";
/// Telemetry API version header name.
pub const HEADER_API_VERSION: &str = "DD-Telemetry-API-Version";
/// Telemetry debug-enabled header name.
pub const HEADER_DEBUG_ENABLED: &str = "DD-Telemetry-Debug-Enabled";
/// Telemetry request type for metric series payloads.
pub const REQUEST_TYPE_GENERATE_METRICS: &str = "generate-metrics";
/// Telemetry API version used by libdatadog telemetry payloads.
pub const TELEMETRY_API_VERSION_V2: &str = "v2";

/// A borrowed Datadog telemetry metrics request.
#[derive(Debug, Clone, Copy)]
pub struct TelemetryMetricsRequest<'a> {
    host: &'a str,
    path: &'a str,
    payload: &'a [u8],
    api_version: &'a str,
    debug_enabled: bool,
    headers: &'a [Header<'a>],
}

impl<'a> TelemetryMetricsRequest<'a> {
    /// Creates a telemetry metrics request targeting the Datadog agent telemetry proxy.
    pub const fn new_agent(host: &'a str, payload: &'a [u8]) -> Self {
        Self {
            host,
            path: AGENT_TELEMETRY_PATH,
            payload,
            api_version: TELEMETRY_API_VERSION_V2,
            debug_enabled: false,
            headers: &[],
        }
    }

    /// Overrides the request path.
    ///
    /// Use [`DIRECT_TELEMETRY_PATH`] for direct intake submissions outside the agent path.
    pub const fn with_path(mut self, path: &'a str) -> Self {
        self.path = path;
        self
    }

    /// Overrides the telemetry API version header value.
    pub const fn with_api_version(mut self, api_version: &'a str) -> Self {
        self.api_version = api_version;
        self
    }

    /// Sets the telemetry debug-enabled header value.
    pub const fn with_debug_enabled(mut self, enabled: bool) -> Self {
        self.debug_enabled = enabled;
        self
    }

    /// Appends endpoint headers such as `dd-api-key` or `x-datadog-test-session-token`.
    pub const fn with_headers(mut self, headers: &'a [Header<'a>]) -> Self {
        self.headers = headers;
        self
    }

    /// Returns the HTTP `Host` header value.
    pub const fn host(&self) -> &'a str {
        self.host
    }

    /// Returns the request path.
    pub const fn path(&self) -> &'a str {
        self.path
    }

    /// Returns the telemetry payload bytes.
    pub const fn payload(&self) -> &'a [u8] {
        self.payload
    }

    /// Returns the number of bytes needed to encode the full request.
    pub fn encoded_len(&self) -> Result<usize, BuildError> {
        let debug_enabled = debug_header_value(self.debug_enabled);
        let telemetry_headers = telemetry_headers(self.api_version, debug_enabled);
        self.request(&telemetry_headers).encoded_len()
    }

    /// Validates all request fields without emitting bytes.
    pub fn validate(&self) -> Result<(), BuildError> {
        let debug_enabled = debug_header_value(self.debug_enabled);
        let telemetry_headers = telemetry_headers(self.api_version, debug_enabled);
        self.request(&telemetry_headers).validate()
    }

    /// Writes the full HTTP request into the supplied sink.
    pub fn submit<S: HttpSink>(&self, sink: &mut S) -> Result<(), SendError<S::Error>> {
        let debug_enabled = debug_header_value(self.debug_enabled);
        let telemetry_headers = telemetry_headers(self.api_version, debug_enabled);
        self.request(&telemetry_headers).write_to(sink)
    }

    /// Encodes the full HTTP request into an owned buffer.
    #[cfg(feature = "alloc")]
    pub fn to_vec(&self) -> Result<Vec<u8>, BuildError> {
        let debug_enabled = debug_header_value(self.debug_enabled);
        let telemetry_headers = telemetry_headers(self.api_version, debug_enabled);
        self.request(&telemetry_headers).to_vec()
    }

    fn request<'request>(
        &'request self,
        telemetry_headers: &'request [Header<'request>],
    ) -> Request<'request>
    where
        'a: 'request,
    {
        Request::new(Method::Post, self.host, self.path)
            .with_body(self.payload)
            .with_content_type(APPLICATION_JSON)
            .with_headers(telemetry_headers)
            .with_extra_headers(self.headers)
    }
}

fn telemetry_headers<'a>(api_version: &'a str, debug_enabled: &'a str) -> [Header<'a>; 3] {
    [
        Header::new_unchecked(HEADER_REQUEST_TYPE, REQUEST_TYPE_GENERATE_METRICS),
        Header::new_unchecked(HEADER_API_VERSION, api_version),
        Header::new_unchecked(HEADER_DEBUG_ENABLED, debug_enabled),
    ]
}

const fn debug_header_value(enabled: bool) -> &'static str {
    if enabled {
        "true"
    } else {
        "false"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BufferTooSmall, FixedBuffer, HttpClient};

    #[test]
    fn telemetry_metrics_request_emits_agent_headers() -> Result<(), SendError<BufferTooSmall>> {
        let headers = [Header::new_unchecked("dd-api-key", "abc123")];
        let request = TelemetryMetricsRequest::new_agent("localhost:8126", br#"{"series":[]}"#)
            .with_debug_enabled(true)
            .with_headers(&headers);
        let mut storage = [0_u8; 512];
        let mut buffer = FixedBuffer::new(&mut storage);

        request.submit(&mut buffer)?;

        let expected = concat!(
            "POST /telemetry/proxy/api/v2/apmtelemetry HTTP/1.1\r\n",
            "Host: localhost:8126\r\n",
            "Content-Length: 13\r\n",
            "Connection: close\r\n",
            "Content-Type: application/json\r\n",
            "DD-Telemetry-Request-Type: generate-metrics\r\n",
            "DD-Telemetry-API-Version: v2\r\n",
            "DD-Telemetry-Debug-Enabled: true\r\n",
            "dd-api-key: abc123\r\n",
            "\r\n",
            r#"{"series":[]}"#
        )
        .as_bytes();
        assert_eq!(buffer.as_slice(), expected);
        assert_eq!(request.encoded_len(), Ok(expected.len()));
        Ok(())
    }

    #[test]
    fn http_client_submits_telemetry_metrics() -> Result<(), SendError<BufferTooSmall>> {
        let headers = [Header::new_unchecked(
            "x-datadog-test-session-token",
            "token",
        )];
        let client = HttpClient::new("localhost:8126").with_default_headers(&headers);
        let mut storage = [0_u8; 512];
        let mut buffer = FixedBuffer::new(&mut storage);

        client.submit_telemetry_metrics(br#"{"series":[]}"#, &mut buffer)?;

        assert!(buffer
            .as_slice()
            .windows(HEADER_REQUEST_TYPE.len())
            .any(|window| window == HEADER_REQUEST_TYPE.as_bytes()));
        assert!(buffer
            .as_slice()
            .windows(headers[0].name().len())
            .any(|window| window == headers[0].name().as_bytes()));
        Ok(())
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn telemetry_alloc_to_vec_contains_payload() -> Result<(), BuildError> {
        let request = TelemetryMetricsRequest::new_agent("localhost:8126", br#"{"series":[]}"#);

        let bytes = request.to_vec()?;

        assert!(bytes.starts_with(b"POST /telemetry/proxy/api/v2/apmtelemetry HTTP/1.1\r\n"));
        assert!(bytes.ends_with(b"\r\n\r\n{\"series\":[]}"));
        Ok(())
    }
}
