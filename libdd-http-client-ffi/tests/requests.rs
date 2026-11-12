// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::*;
use httpmock::prelude::*;
use libdd_common_ffi::CharSlice;
use libdd_common_ffi::slice::ByteSlice;
use libdd_http_client_ffi::*;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::thread;

/// A client for requests that fail before anything is sent.
fn offline_client() -> Client {
    Cfg::new("http://localhost").build().expect("client")
}

#[test]
fn every_method_is_supported() {
    let server = MockServer::start();
    let client = Cfg::new(server.base_url()).build().expect("client");

    for (method, mock_method) in [
        (HttpMethod::Get, Method::GET),
        (HttpMethod::Post, Method::POST),
        (HttpMethod::Put, Method::PUT),
        (HttpMethod::Delete, Method::DELETE),
        (HttpMethod::Head, Method::HEAD),
        (HttpMethod::Patch, Method::PATCH),
        (HttpMethod::Options, Method::OPTIONS),
    ] {
        let mock = server.mock(|when, then| {
            when.method(mock_method).path("/m");
            then.status(200).body("ok");
        });

        let response = client.send(&Req::new(method, "/m")).expect("send");

        assert_eq!(response.status(), 200);
        mock.assert();
    }
}

#[test]
fn post_sends_a_binary_body_and_its_headers_unchanged() {
    let server = MockServer::start();
    // The body is not valid UTF-8, so only its length can be matched.
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/v0.4/traces")
            .header("content-type", "application/msgpack")
            .header("x-datadog-trace-count", "3")
            .header("content-length", "7");
        then.status(200);
    });
    let client = Cfg::new(server.base_url()).build().expect("client");

    let request = Req::post("/v0.4/traces")
        .header("Content-Type", "application/msgpack")
        .header("X-Datadog-Trace-Count", "3")
        .body(vec![0x00, 0xff, 0x92, 0xa1, b'a', 0x00, 0x80]);
    client.send(&request).expect("send");

    mock.assert();
}

#[test]
fn post_sends_a_payload_as_large_as_a_trace_chunk() {
    let size = 5 * 1024 * 1024;
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/big")
            .header("content-length", size.to_string());
        then.status(200);
    });
    let client = Cfg::new(server.base_url())
        .timeout_ms(30_000)
        .build()
        .expect("client");

    let response = client
        .send(&Req::post("/big").body(vec![0xab; size]))
        .expect("send");

    assert_eq!(response.status(), 200);
    mock.assert();
}

#[test]
fn a_relative_url_is_appended_to_the_base_url_with_its_query() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path("/v0.7/config")
            .query_param("a", "1")
            .query_param("b", "two");
        then.status(200);
    });
    let client = Cfg::new(server.base_url()).build().expect("client");

    client
        .send(&Req::get("/v0.7/config?a=1&b=two"))
        .expect("send");

    mock.assert();
}

#[test]
fn an_absolute_url_ignores_the_base_url() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.path("/abs");
        then.status(200);
    });
    // The base URL is unreachable: only the absolute URL can succeed.
    let client = Cfg::for_addr(refused_addr()).build().expect("client");

    let response = client.send(&Req::get(server.url("/abs"))).expect("send");

    assert_eq!(response.status(), 200);
    mock.assert();
}

#[test]
fn default_headers_are_sent_with_every_request() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.header("datadog-meta-lang", "ruby")
            .header("datadog-meta-tracer-version", "2.0.0");
        then.status(200);
    });
    let client = Cfg::new(server.base_url())
        .default_header("Datadog-Meta-Lang", "ruby")
        .default_header("Datadog-Meta-Tracer-Version", "2.0.0")
        .build()
        .expect("client");

    client.send(&Req::get("/a")).expect("first");
    client.send(&Req::get("/b")).expect("second");

    mock.assert_calls(2);
}

#[test]
fn request_headers_replace_default_headers_case_insensitively() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.header("x-override", "request")
            .header("x-keep", "default")
            .header("x-extra", "1");
        then.status(200);
    });
    let client = Cfg::new(server.base_url())
        .default_header("X-Override", "default")
        .default_header("X-Keep", "default")
        .build()
        .expect("client");

    client
        .send(
            &Req::get("/h")
                .header("x-override", "request")
                .header("X-Extra", "1"),
        )
        .expect("send");

    mock.assert();
}

#[test]
fn an_invalid_header_name_is_rejected() {
    let error = offline_client()
        .send(&Req::get("/h").header("bad header", "v"))
        .err()
        .expect("invalid header name");

    // reqwest reports it as an I/O error.
    if !cfg!(feature = "reqwest-backend") {
        assert_eq!(error.code, HttpClientErrorCode::InvalidArgument);
    }
}

// The two backends reject different malformed requests, so each gets its own test.
#[cfg(not(feature = "reqwest-backend"))]
#[test]
fn a_malformed_absolute_url_is_an_invalid_argument() {
    let error = offline_client()
        .send(&Req::get("http://exa mple/x"))
        .err()
        .expect("malformed url");

    assert_eq!(error.code, HttpClientErrorCode::InvalidArgument);
}

#[cfg(feature = "reqwest-backend")]
#[test]
fn an_invalid_multipart_content_type_is_an_invalid_argument() {
    let error = offline_client()
        .send(&Req::post("/x").part(Part::new("p", b"v").content_type("not a mime")))
        .err()
        .expect("invalid content type");

    assert_eq!(error.code, HttpClientErrorCode::InvalidArgument);
}

#[test]
fn a_multipart_body_is_encoded_with_its_parts() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/debugger/v1/diagnostics")
            .header_prefix("content-type", "multipart/form-data; boundary=")
            .body_includes(r#"name="event""#)
            .body_includes(r#"filename="event.json""#)
            .body_includes("application/json")
            .body_includes(r#"{"a":1}"#)
            .body_includes(r#"name="file""#)
            .body_includes(r#"filename="archive.zip""#)
            .body_includes("archive-content");
        then.status(200);
    });
    let client = Cfg::new(server.base_url()).build().expect("client");

    let request = Req::post("/debugger/v1/diagnostics")
        .part(
            Part::new("event", br#"{"a":1}"#)
                .filename("event.json")
                .content_type("application/json"),
        )
        .part(
            Part::new("file", b"archive-content")
                .filename("archive.zip")
                .content_type("application/octet-stream"),
        );
    client.send(&request).expect("send");

    mock.assert();
}

#[test]
fn a_body_and_multipart_parts_are_mutually_exclusive() {
    let error = offline_client()
        .send(
            &Req::post("/x")
                .body(b"raw".to_vec())
                .part(Part::new("p", b"v")),
        )
        .err()
        .expect("body + multipart");

    assert_eq!(error.code, HttpClientErrorCode::InvalidArgument);
}

#[test]
fn send_rejects_null_pointers() {
    let client = offline_client();
    let request = Req::get("/x");

    unsafe {
        let mut out: *mut HttpResponse = null_mut();
        let no_client = request.with_ffi(|r| ddog_http_client_send(null(), r, null(), &mut out));
        assert_eq!(
            take_error(no_client).unwrap_err().code,
            HttpClientErrorCode::InvalidArgument
        );

        let no_request = ddog_http_client_send(client.0, null(), null(), &mut out);
        assert_eq!(
            take_error(no_request).unwrap_err().code,
            HttpClientErrorCode::InvalidArgument
        );

        let no_out = request.with_ffi(|r| ddog_http_client_send(client.0, r, null(), null_mut()));
        assert_eq!(
            take_error(no_out).unwrap_err().code,
            HttpClientErrorCode::InvalidArgument
        );
    }
}

#[test]
fn send_rejects_a_url_that_is_not_utf8() {
    let client = offline_client();
    let invalid = [b'/', 0xff, 0xfe];
    let request = HttpRequest {
        method: HttpMethod::Get,
        url: CharSlice::from_bytes(&invalid),
        headers: Default::default(),
        body: ByteSlice::default(),
        multipart: Default::default(),
        timeout_ms: 0,
    };

    let mut out: *mut HttpResponse = null_mut();
    let error = take_error(unsafe { ddog_http_client_send(client.0, &request, null(), &mut out) })
        .expect_err("non-UTF-8 url");

    assert_eq!(error.code, HttpClientErrorCode::InvalidArgument);
}

#[test]
fn one_client_can_be_used_from_several_threads_at_once() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path_prefix("/t");
        then.status(200).body("ok");
    });
    let client = Arc::new(
        Cfg::new(server.base_url())
            .timeout_ms(10_000)
            .build()
            .expect("client"),
    );

    let threads: Vec<_> = (0..8)
        .map(|i| {
            let client = client.clone();
            thread::spawn(move || {
                for j in 0..5 {
                    let response = client
                        .send(&Req::post(format!("/t{i}-{j}")).body(vec![i as u8; 64]))
                        .expect("send");
                    assert_eq!(response.status(), 200);
                    assert_eq!(response.body(), b"ok");
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().expect("sender thread panicked");
    }

    mock.assert_calls(40);
}
