// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::*;
use libdd_common_ffi::CharSlice;
use libdd_common_ffi::slice::ByteSlice;
use libdd_http_client_ffi::*;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn get_sends_the_method_the_path_and_no_body() {
    let server = spawn_tcp(respond_ok);
    let client = server.cfg().build().expect("client");

    let response = client.send(&Req::get("/info")).expect("send");

    let received = server.recv();
    assert_eq!(received.method, "GET");
    assert_eq!(received.target, "/info");
    assert!(received.body.is_empty());
    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), b"ok");
}

#[test]
fn every_method_is_supported() {
    let server = spawn_tcp(respond_ok);
    let client = server.cfg().build().expect("client");

    for (method, name) in [
        (HttpMethod::Get, "GET"),
        (HttpMethod::Post, "POST"),
        (HttpMethod::Put, "PUT"),
        (HttpMethod::Delete, "DELETE"),
        (HttpMethod::Head, "HEAD"),
        (HttpMethod::Patch, "PATCH"),
        (HttpMethod::Options, "OPTIONS"),
    ] {
        let response = client.send(&Req::new(method, "/m")).expect(name);
        assert_eq!(response.status(), 200, "{name}");
        assert_eq!(server.recv().method, name);
    }
}

#[test]
fn post_sends_a_binary_body_and_its_headers_unchanged() {
    let server = spawn_tcp(respond_ok);
    let client = server.cfg().build().expect("client");
    let body = vec![0x00, 0xff, 0x92, 0xa1, b'a', 0x00, 0x80];

    let request = Req::post("/v0.4/traces")
        .header("Content-Type", "application/msgpack")
        .header("X-Datadog-Trace-Count", "3")
        .body(body.clone());
    client.send(&request).expect("send");

    let received = server.recv();
    assert_eq!(received.method, "POST");
    assert_eq!(received.body, body);
    assert_eq!(received.header("content-type"), Some("application/msgpack"));
    assert_eq!(received.header("x-datadog-trace-count"), Some("3"));
    assert_eq!(
        received.header("content-length"),
        Some(body.len().to_string().as_str())
    );
}

#[test]
fn post_sends_a_payload_as_large_as_a_trace_chunk() {
    let server = spawn_tcp(respond_ok);
    let client = server.cfg().timeout_ms(30_000).build().expect("client");
    let body = vec![0xab; 5 * 1024 * 1024];

    let response = client
        .send(&Req::post("/big").body(body.clone()))
        .expect("send");

    assert_eq!(response.status(), 200);
    assert_eq!(server.recv().body.len(), body.len());
}

#[test]
fn a_relative_url_is_appended_to_the_base_url_with_its_query() {
    let server = spawn_tcp(respond_ok);
    let client = server.cfg().build().expect("client");

    client
        .send(&Req::get("/v0.7/config?a=1&b=two"))
        .expect("send");

    assert_eq!(server.recv().target, "/v0.7/config?a=1&b=two");
}

#[test]
fn the_trailing_slash_of_the_base_url_is_not_duplicated() {
    let server = spawn_tcp(respond_ok);
    let client = Cfg::new(format!("{}/", server.url("")))
        .build()
        .expect("client");

    client.send(&Req::get("/p")).expect("send");

    assert_eq!(server.recv().target, "/p");
}

#[test]
fn the_path_prefix_of_the_base_url_is_kept() {
    let server = spawn_tcp(respond_ok);
    let client = Cfg::new(server.url("/prefix")).build().expect("client");

    client.send(&Req::get("/p")).expect("send");

    assert_eq!(server.recv().target, "/prefix/p");
}

#[test]
fn an_absolute_url_ignores_the_base_url() {
    let server = spawn_tcp(respond_ok);
    // The base URL is unreachable: only the absolute URL can succeed.
    let client = Cfg::for_addr(refused_addr()).build().expect("client");

    let response = client.send(&Req::get(server.url("/abs"))).expect("send");

    assert_eq!(response.status(), 200);
    assert_eq!(server.recv().target, "/abs");
}

#[test]
fn a_url_that_is_neither_absolute_nor_a_path_is_rejected() {
    let server = spawn_tcp(respond_ok);
    let client = server.cfg().build().expect("client");

    for url in ["v0.4/traces", "", "ftp://localhost/x", "//host/x"] {
        let error = client.send(&Req::get(url)).err().expect(url);
        assert_eq!(error.code, HttpClientErrorCode::InvalidArgument, "{url:?}");
    }
    assert!(server.nothing_within(Duration::from_millis(200)));
}

// The two backends reject different malformed requests, so each gets its own test.
#[cfg(not(feature = "reqwest-backend"))]
#[test]
fn a_malformed_absolute_url_is_an_invalid_argument() {
    let server = spawn_tcp(respond_ok);
    let client = server.cfg().build().expect("client");

    let error = client
        .send(&Req::get("http://exa mple/x"))
        .err()
        .expect("malformed url");

    assert_eq!(
        error.code,
        HttpClientErrorCode::InvalidArgument,
        "{}",
        error.message
    );
}

#[cfg(feature = "reqwest-backend")]
#[test]
fn an_invalid_multipart_content_type_is_an_invalid_argument() {
    let server = spawn_tcp(respond_ok);
    let client = server.cfg().build().expect("client");

    let error = client
        .send(&Req::post("/x").part(Part::new("p", b"v").content_type("not a mime")))
        .err()
        .expect("invalid content type");

    assert_eq!(
        error.code,
        HttpClientErrorCode::InvalidArgument,
        "{}",
        error.message
    );
}

#[test]
fn default_headers_are_sent_with_every_request() {
    let server = spawn_tcp(respond_ok);
    let client = server
        .cfg()
        .default_header("Datadog-Meta-Lang", "ruby")
        .default_header("Datadog-Meta-Tracer-Version", "2.0.0")
        .build()
        .expect("client");

    client.send(&Req::get("/a")).expect("first");
    client.send(&Req::get("/b")).expect("second");

    for _ in 0..2 {
        let received = server.recv();
        assert_eq!(received.header("datadog-meta-lang"), Some("ruby"));
        assert_eq!(
            received.header("datadog-meta-tracer-version"),
            Some("2.0.0")
        );
    }
}

#[test]
fn request_headers_replace_default_headers_case_insensitively() {
    let server = spawn_tcp(respond_ok);
    let client = server
        .cfg()
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

    let received = server.recv();
    assert_eq!(received.header_values("x-override"), vec!["request"]);
    assert_eq!(received.header_values("x-keep"), vec!["default"]);
    assert_eq!(received.header_values("x-extra"), vec!["1"]);
}

#[test]
fn an_invalid_header_name_is_rejected() {
    let server = spawn_tcp(respond_ok);
    let client = server.cfg().build().expect("client");

    let error = client
        .send(&Req::get("/h").header("bad header", "v"))
        .err()
        .expect("invalid header name");

    assert_eq!(error.code, HttpClientErrorCode::InvalidArgument);
    assert!(server.nothing_within(Duration::from_millis(200)));
}

#[test]
fn a_multipart_body_is_encoded_with_its_parts() {
    let server = spawn_tcp(respond_ok);
    let client = server.cfg().build().expect("client");
    let archive = vec![0x50, 0x4b, 0x03, 0x04, 0x00, 0xff];

    let request = Req::post("/debugger/v1/diagnostics")
        .part(
            Part::new("event", br#"{"a":1}"#)
                .filename("event.json")
                .content_type("application/json"),
        )
        .part(
            Part::new("file", &archive)
                .filename("archive.zip")
                .content_type("application/octet-stream"),
        );
    client.send(&request).expect("send");

    let received = server.recv();
    let content_type = received.header("content-type").expect("content-type");
    assert!(
        content_type.starts_with("multipart/form-data; boundary="),
        "{content_type}"
    );
    assert!(contains(&received.body, br#"name="event""#));
    assert!(contains(&received.body, br#"filename="event.json""#));
    assert!(contains(&received.body, b"application/json"));
    assert!(contains(&received.body, br#"{"a":1}"#));
    assert!(contains(&received.body, br#"name="file""#));
    assert!(contains(&received.body, br#"filename="archive.zip""#));
    assert!(contains(&received.body, &archive));
}

#[test]
fn a_body_and_multipart_parts_are_mutually_exclusive() {
    let server = spawn_tcp(respond_ok);
    let client = server.cfg().build().expect("client");

    let error = client
        .send(
            &Req::post("/x")
                .body(b"raw".to_vec())
                .part(Part::new("p", b"v")),
        )
        .err()
        .expect("body + multipart");

    assert_eq!(error.code, HttpClientErrorCode::InvalidArgument);
    assert!(server.nothing_within(Duration::from_millis(200)));
}

#[test]
fn a_multipart_part_needs_a_name() {
    let server = spawn_tcp(respond_ok);
    let client = server.cfg().build().expect("client");

    let error = client
        .send(&Req::post("/x").part(Part::new("", b"v")))
        .err()
        .expect("nameless part");

    assert_eq!(error.code, HttpClientErrorCode::InvalidArgument);
}

#[test]
fn send_rejects_null_pointers() {
    let server = spawn_tcp(respond_ok);
    let client = server.cfg().build().expect("client");
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
    assert!(server.nothing_within(Duration::from_millis(200)));
}

#[test]
fn send_rejects_a_url_that_is_not_utf8() {
    let server = spawn_tcp(respond_ok);
    let client = server.cfg().build().expect("client");
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
    let server = spawn_tcp(respond_ok);
    let client = Arc::new(server.cfg().timeout_ms(10_000).build().expect("client"));

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

    let mut targets: Vec<String> = (0..40).map(|_| server.recv().target).collect();
    targets.sort();
    let mut expected: Vec<String> = (0..8)
        .flat_map(|i| (0..5).map(move |j| format!("/t{i}-{j}")))
        .collect();
    expected.sort();
    assert_eq!(targets, expected);
}
