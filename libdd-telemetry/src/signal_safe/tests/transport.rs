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
