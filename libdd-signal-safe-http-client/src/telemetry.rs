// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use reqwless::{
    headers::ContentType,
    request::{Request, RequestBuilder},
};

/// A reqwless extra-header tuple.
pub type Header<'a> = (&'a str, &'a str);

/// Agent telemetry proxy path for APM telemetry payloads.
pub const AGENT_TELEMETRY_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";
/// Direct intake telemetry path for APM telemetry payloads.
pub const DIRECT_TELEMETRY_PATH: &str = "/api/v2/apmtelemetry";
/// JSON content type emitted for telemetry payloads.
pub const APPLICATION_JSON: &str = "application/json";
/// `Connection: close` header value for one-shot signal-handler submissions.
pub const CONNECTION_CLOSE: Header<'static> = ("Connection", "close");
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

/// Builds the default telemetry headers for a `generate-metrics` payload.
///
/// The returned slice can be passed directly to `reqwless` through
/// [`telemetry_metrics_request`]. Callers that need endpoint headers such as `dd-api-key` should
/// append those to their own reqwless header storage and pass that combined slice instead.
pub const fn telemetry_metrics_headers<'a>(
    api_version: &'a str,
    debug_enabled: bool,
) -> [Header<'a>; 4] {
    [
        CONNECTION_CLOSE,
        (HEADER_REQUEST_TYPE, REQUEST_TYPE_GENERATE_METRICS),
        (HEADER_API_VERSION, api_version),
        (HEADER_DEBUG_ENABLED, debug_header_value(debug_enabled)),
    ]
}

/// Builds a reqwless `POST` request for a Datadog telemetry metrics payload.
///
/// This function does not validate, encode, or write HTTP bytes itself. It only applies Datadog
/// telemetry defaults to reqwless's low-level request builder.
pub fn telemetry_metrics_request<'a>(
    host: &'a str,
    path: &'a str,
    payload: &'a [u8],
    headers: &'a [Header<'a>],
) -> Request<'a, &'a [u8]> {
    reqwless::request::Request::post(path)
        .host(host)
        .content_type(ContentType::ApplicationJson)
        .headers(headers)
        .body(payload)
        .build()
}

/// Builds a reqwless `POST` request for the Datadog agent telemetry proxy.
pub fn agent_telemetry_metrics_request<'a>(
    host: &'a str,
    payload: &'a [u8],
    headers: &'a [Header<'a>],
) -> Request<'a, &'a [u8]> {
    telemetry_metrics_request(host, AGENT_TELEMETRY_PATH, payload, headers)
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
    use core::{
        future::Future,
        ptr,
        task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
    };

    use reqwless::request::RequestBody as _;

    use super::*;

    #[test]
    fn telemetry_headers_are_reqwless_header_tuples() {
        assert_eq!(
            telemetry_metrics_headers(TELEMETRY_API_VERSION_V2, true),
            [
                ("Connection", "close"),
                ("DD-Telemetry-Request-Type", "generate-metrics"),
                ("DD-Telemetry-API-Version", "v2"),
                ("DD-Telemetry-Debug-Enabled", "true"),
            ]
        );
    }

    #[test]
    fn telemetry_request_is_reqwless_request() {
        let payload = br#"{"series":[]}"#;
        let headers = telemetry_metrics_headers(TELEMETRY_API_VERSION_V2, true);
        let request = agent_telemetry_metrics_request("localhost:8126", payload, &headers);
        let mut writer = VecWriter::default();

        block_on_ready(request.write_header(&mut writer))
            .expect("reqwless write_header should be ready")
            .expect("reqwless write_header should succeed");
        block_on_ready(payload.as_slice().write(&mut writer))
            .expect("reqwless body write should be ready")
            .expect("reqwless body write should succeed");

        let expected = concat!(
            "POST /telemetry/proxy/api/v2/apmtelemetry HTTP/1.1\r\n",
            "Host: localhost:8126\r\n",
            "Content-Type: application/json\r\n",
            "Content-Length: 13\r\n",
            "Connection: close\r\n",
            "DD-Telemetry-Request-Type: generate-metrics\r\n",
            "DD-Telemetry-API-Version: v2\r\n",
            "DD-Telemetry-Debug-Enabled: true\r\n",
            "\r\n",
            r#"{"series":[]}"#
        )
        .as_bytes();
        assert_eq!(writer.bytes, expected);
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct TestError;

    impl embedded_io::Error for TestError {
        fn kind(&self) -> embedded_io::ErrorKind {
            embedded_io::ErrorKind::Other
        }
    }

    #[derive(Default)]
    struct VecWriter {
        bytes: std::vec::Vec<u8>,
    }

    impl embedded_io::ErrorType for VecWriter {
        type Error = TestError;
    }

    impl embedded_io_async::Write for VecWriter {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    fn block_on_ready<F: Future>(future: F) -> Result<F::Output, ()> {
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut future = core::pin::pin!(future);

        match future.as_mut().poll(&mut cx) {
            Poll::Ready(output) => Ok(output),
            Poll::Pending => Err(()),
        }
    }

    fn noop_waker() -> Waker {
        const VTABLE: RawWakerVTable = RawWakerVTable::new(
            noop_waker_clone,
            noop_waker_wake,
            noop_waker_wake_by_ref,
            noop_waker_drop,
        );

        // SAFETY: The raw waker uses a null data pointer that is never dereferenced. All vtable
        // operations are no-ops and cloning returns another equivalent raw waker.
        unsafe { Waker::from_raw(RawWaker::new(ptr::null(), &VTABLE)) }
    }

    unsafe fn noop_waker_clone(_data: *const ()) -> RawWaker {
        const VTABLE: RawWakerVTable = RawWakerVTable::new(
            noop_waker_clone,
            noop_waker_wake,
            noop_waker_wake_by_ref,
            noop_waker_drop,
        );
        RawWaker::new(ptr::null(), &VTABLE)
    }

    unsafe fn noop_waker_wake(_data: *const ()) {}

    unsafe fn noop_waker_wake_by_ref(_data: *const ()) {}

    unsafe fn noop_waker_drop(_data: *const ()) {}
}
