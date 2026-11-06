// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! What the caller gets back: statuses as data, headers, bodies, and the lifetime of a response.

mod common;

use common::*;
use libdd_common_ffi::CharSlice;
use libdd_http_client_ffi::*;

#[test]
fn error_statuses_are_returned_as_data_with_their_body_and_headers() {
    // dd-trace-rb falls back to another API version on 404/415 and needs the body of every status.
    for (status, body) in [(404, "not here"), (415, "unsupported"), (500, "boom")] {
        let server = spawn_tcp(move |_| {
            Behavior::Respond(http_response(
                status,
                &[("X-Reason", "test")],
                body.as_bytes(),
            ))
        });
        let client = server.cfg().build().expect("client");

        let response = client
            .send(&Req::post("/v0.4/traces").body(b"x".to_vec()))
            .unwrap_or_else(|e| panic!("status {status} must not be an error: {e:?}"));

        assert_eq!(response.status(), status);
        assert_eq!(response.body(), body.as_bytes());
        assert_eq!(response.header("x-reason").as_deref(), Some("test"));
    }
}

#[test]
fn an_empty_response_has_an_empty_body() {
    let server = spawn_tcp(|_| Behavior::Respond(http_response(204, &[], b"")));
    let client = server.cfg().build().expect("client");

    let response = client.send(&Req::get("/none")).expect("send");

    assert_eq!(response.status(), 204);
    assert!(response.body().is_empty());
}

#[test]
fn a_binary_body_is_returned_unchanged() {
    let payload = vec![0x00, 0xff, 0x01, 0xfe, 0x00];
    let expected = payload.clone();
    let server = spawn_tcp(move |_| Behavior::Respond(http_response(200, &[], &payload)));
    let client = server.cfg().build().expect("client");

    let response = client.send(&Req::get("/bin")).expect("send");

    assert_eq!(response.body(), expected);
}

#[test]
fn header_get_is_case_insensitive() {
    let server = spawn_tcp(|_| {
        Behavior::Respond(http_response(
            200,
            &[
                ("Datadog-Container-Tags-Hash", "abc123"),
                ("ETag", "\"v1\""),
            ],
            b"",
        ))
    });
    let client = server.cfg().build().expect("client");

    let response = client.send(&Req::get("/info")).expect("send");

    for name in [
        "Datadog-Container-Tags-Hash",
        "datadog-container-tags-hash",
        "DATADOG-CONTAINER-TAGS-HASH",
    ] {
        assert_eq!(response.header(name).as_deref(), Some("abc123"), "{name}");
    }
    assert_eq!(response.header("etag").as_deref(), Some("\"v1\""));
}

#[test]
fn header_get_returns_an_empty_slice_for_a_missing_header() {
    let server = spawn_tcp(respond_ok);
    let client = server.cfg().build().expect("client");

    let response = client.send(&Req::get("/info")).expect("send");

    assert_eq!(response.header("x-not-there"), None);
}

#[test]
fn header_at_iterates_in_order_and_keeps_repeated_names() {
    let server = spawn_tcp(|_| {
        Behavior::Respond(http_response(
            200,
            &[
                ("Set-Cookie", "a=1"),
                ("X-Between", "b"),
                ("Set-Cookie", "c=3"),
            ],
            b"",
        ))
    });
    let client = server.cfg().build().expect("client");

    let response = client.send(&Req::get("/cookies")).expect("send");

    let headers = response.headers();
    assert_eq!(response.header_count(), headers.len());
    let cookies: Vec<&str> = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
        .map(|(_, value)| value.as_str())
        .collect();
    assert_eq!(cookies, vec!["a=1", "c=3"]);
    // Header_get returns the first one.
    assert_eq!(response.header("set-cookie").as_deref(), Some("a=1"));
    // Order is the order received.
    let position = |wanted: &str| headers.iter().position(|(_, v)| v == wanted).expect(wanted);
    assert!(position("a=1") < position("b") && position("b") < position("c=3"));
}

#[test]
fn header_at_out_of_range_returns_false_and_writes_nothing() {
    let server = spawn_tcp(respond_ok);
    let client = server.cfg().build().expect("client");
    let response = client.send(&Req::get("/info")).expect("send");

    let mut name = CharSlice::from("untouched");
    let mut value = CharSlice::from("untouched");
    // SAFETY: the response is live and the out pointers are valid for writes.
    let found = unsafe {
        ddog_http_response_header_at(response.0, response.header_count(), &mut name, &mut value)
    };

    assert!(!found);
    assert_eq!(name, CharSlice::from("untouched"));
    assert_eq!(value, CharSlice::from("untouched"));
}

#[test]
fn a_response_stays_valid_after_its_client_is_dropped() {
    let server =
        spawn_tcp(|_| Behavior::Respond(http_response(200, &[("X-Kept", "yes")], b"still here")));
    let client = server.cfg().build().expect("client");
    let response = client.send(&Req::get("/keep")).expect("send");

    drop(client);

    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), b"still here");
    assert_eq!(response.header("x-kept").as_deref(), Some("yes"));
}

#[test]
fn dropping_a_null_response_is_a_no_op() {
    // SAFETY: `None` (NULL) is part of the documented contract of `drop`.
    unsafe { ddog_http_response_drop(None) };
}
