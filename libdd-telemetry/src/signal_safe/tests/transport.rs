// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use core::convert::Infallible;
use reqwless::client::{HttpConnection, HttpResource};

use super::super::*;

struct FakeConnection {
    response: &'static [u8],
    response_offset: usize,
    request: Vec<u8>,
}

impl embedded_io::ErrorType for FakeConnection {
    type Error = Infallible;
}

impl embedded_io_async::Read for FakeConnection {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        let remaining = &self.response[self.response_offset..];
        let length = remaining.len().min(buffer.len());
        buffer[..length].copy_from_slice(&remaining[..length]);
        self.response_offset += length;
        Ok(length)
    }
}

impl embedded_io_async::Write for FakeConnection {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.request.extend_from_slice(buffer);
        Ok(buffer.len())
    }
}

fn empty_request<'a>(application: &'a Application, host: &'a Host) -> MetricsRequest<'a> {
    MetricsRequest {
        tracer_time: 1,
        runtime_id: "runtime",
        seq_id: 1,
        application,
        host,
        origin: None,
        series: &[],
    }
}

#[tokio::test]
async fn sends_encoded_metrics_and_accepts_success_statuses() {
    let application = Application {
        language_name: "rust".to_string(),
        tracer_version: "1.0.0".to_string(),
        ..Application::default()
    };
    let host = Host::default();
    let request = empty_request(&application, &host);
    let mut connection = FakeConnection {
        response: b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n",
        response_offset: 0,
        request: Vec::new(),
    };
    let mut body_buffer = [0_u8; 1_024];
    let mut response_buffer = [0_u8; 256];
    let status = {
        let mut resource = HttpResource {
            conn: HttpConnection::Plain(&mut connection),
            host: "localhost",
            base_path: "",
        };
        send_metrics(
            &mut resource,
            "/telemetry/proxy/api/v2/apmtelemetry",
            &request,
            &[("DD-API-KEY", "secret")],
            &mut body_buffer,
            &mut response_buffer,
        )
        .await
        .unwrap()
    };

    assert_eq!(status, 202);
    let written = String::from_utf8(connection.request).unwrap();
    assert!(written.starts_with("POST /telemetry/proxy/api/v2/apmtelemetry HTTP/1.1\r\n"));
    assert!(written.contains("dd-telemetry-request-type: generate-metrics\r\n"));
    assert!(written.contains("dd-client-library-language: rust\r\n"));
    assert!(written.contains("Connection: close\r\n"));
    assert!(written.contains("DD-API-KEY: secret\r\n"));
    assert!(written.ends_with("\"series\":[]}}"));
}

#[tokio::test]
async fn rejects_unsuccessful_statuses() {
    let application = Application {
        language_name: "rust".to_string(),
        tracer_version: "1.0.0".to_string(),
        ..Application::default()
    };
    let host = Host::default();
    let request = empty_request(&application, &host);
    let mut connection = FakeConnection {
        response: b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n",
        response_offset: 0,
        request: Vec::new(),
    };
    let mut resource = HttpResource {
        conn: HttpConnection::Plain(&mut connection),
        host: "localhost",
        base_path: "",
    };
    let mut body_buffer = [0_u8; 1_024];
    let mut response_buffer = [0_u8; 256];

    assert!(matches!(
        send_metrics(
            &mut resource,
            "/telemetry/proxy/api/v2/apmtelemetry",
            &request,
            &[],
            &mut body_buffer,
            &mut response_buffer,
        )
        .await,
        Err(Error::UnexpectedStatus(500))
    ));
}

#[tokio::test]
async fn rejects_too_many_extra_headers() {
    let application = Application::default();
    let host = Host::default();
    let request = empty_request(&application, &host);
    let mut connection = FakeConnection {
        response: b"",
        response_offset: 0,
        request: Vec::new(),
    };
    let mut resource = HttpResource {
        conn: HttpConnection::Plain(&mut connection),
        host: "localhost",
        base_path: "",
    };
    let extra_headers = [("x-header", "value"); MAX_EXTRA_HEADERS + 1];
    let mut body_buffer = [0_u8; 1_024];
    let mut response_buffer = [0_u8; 256];

    assert!(matches!(
        send_metrics(
            &mut resource,
            "/telemetry/proxy/api/v2/apmtelemetry",
            &request,
            &extra_headers,
            &mut body_buffer,
            &mut response_buffer,
        )
        .await,
        Err(Error::TooManyHeaders)
    ));
    assert!(connection.request.is_empty());
}

async fn submit_headers(
    application: &Application,
    extra_headers: &[(&str, &str)],
) -> (Result<u16, Error>, Vec<u8>) {
    let host = Host::default();
    let request = empty_request(application, &host);
    let mut connection = FakeConnection {
        response: b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n",
        response_offset: 0,
        request: Vec::new(),
    };
    let mut body_buffer = [0_u8; 1_024];
    let mut response_buffer = [0_u8; 256];
    let result = {
        let mut resource = HttpResource {
            conn: HttpConnection::Plain(&mut connection),
            host: "localhost",
            base_path: "",
        };
        send_metrics(
            &mut resource,
            "/telemetry/proxy/api/v2/apmtelemetry",
            &request,
            extra_headers,
            &mut body_buffer,
            &mut response_buffer,
        )
        .await
    };
    (result, connection.request)
}

#[tokio::test]
async fn rejects_invalid_extra_header_names_before_writing() {
    for name in [
        "",
        "has space",
        "has\ttab",
        "name:value",
        "header\"quote",
        "header\r\nInjected",
        "header\0",
        "header\x7f",
        "héadér",
        "()<>@,;:\\\"/[]?={}",
    ] {
        let (result, written) = submit_headers(&Application::default(), &[(name, "value")]).await;
        assert!(matches!(result, Err(Error::InvalidHeaderName)), "{name:?}");
        assert!(written.is_empty(), "{name:?}");
    }
}

#[tokio::test]
async fn rejects_invalid_extra_header_values_before_writing() {
    for byte in (0..=0x1f).chain(core::iter::once(0x7f)) {
        if byte == b'\t' {
            continue;
        }
        let value = char::from(byte).to_string();
        let (result, written) =
            submit_headers(&Application::default(), &[("x-header", &value)]).await;
        assert!(
            matches!(result, Err(Error::InvalidHeaderValue)),
            "{byte:#x}"
        );
        assert!(written.is_empty(), "{byte:#x}");
    }
}

#[tokio::test]
async fn rejects_invalid_application_header_values_before_writing() {
    for value in [
        "rust\r\nX-Injected: yes",
        "bad\n",
        "bad\r",
        "bad\0",
        "bad\x7f",
    ] {
        for is_language in [true, false] {
            let mut application = Application::default();
            if is_language {
                application.language_name = value.to_string();
            } else {
                application.tracer_version = value.to_string();
            }
            let (result, written) = submit_headers(&application, &[]).await;
            assert!(
                matches!(result, Err(Error::InvalidHeaderValue)),
                "{value:?}"
            );
            assert!(written.is_empty(), "{value:?}");
        }
    }
}

#[tokio::test]
async fn accepts_valid_http_header_characters() {
    let (result, written) = submit_headers(
        &Application::default(),
        &[("X-!#$%&'*+-.^_`|~", " \tvisible é"), ("x-empty", "")],
    )
    .await;
    assert!(matches!(result, Ok(202)));
    let written = String::from_utf8(written).unwrap();
    assert!(written.contains("X-!#$%&'*+-.^_`|~:  \tvisible é\r\n"));
    assert!(written.contains("x-empty: \r\n"));
}

#[cfg(feature = "std")]
#[test]
fn header_value_validation_matches_the_existing_http_type() {
    for byte in 0..=u8::MAX {
        let text = char::from(byte).to_string();
        assert_eq!(
            headers::validate(&[("x-header", &text)]).is_ok(),
            http::HeaderValue::from_str(&text).is_ok(),
            "header value {byte:#x}",
        );
    }
}
