// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::*;
use httpmock::prelude::*;
use libdd_http_client_ffi::*;

/// A server that answers every request as `configure` says, and a client for it.
fn serve(configure: impl FnOnce(httpmock::Then)) -> (MockServer, Client) {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.path_prefix("/");
        configure(then);
    });
    let client = Cfg::new(server.base_url()).build().expect("client");
    (server, client)
}

#[test]
#[cfg_attr(miri, ignore)]
fn error_statuses_are_returned_as_data_with_their_body_and_headers() {
    let server = MockServer::start();
    let client = Cfg::new(server.base_url()).build().expect("client");

    for (status, body) in [(404u16, "not here"), (415, "unsupported"), (500, "boom")] {
        server.mock(|when, then| {
            when.path(format!("/{status}"));
            then.status(status).header("X-Reason", "test").body(body);
        });

        let response = client
            .send(&Req::post(format!("/{status}")).body(b"x".to_vec()))
            .unwrap_or_else(|e| panic!("status {status} must not be an error: {e:?}"));

        assert_eq!(response.status(), status);
        assert_eq!(response.body(), body.as_bytes());
        assert_eq!(response.header("x-reason").as_deref(), Some("test"));
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn an_empty_response_has_an_empty_body() {
    let (_server, client) = serve(|then| {
        then.status(204);
    });

    let response = client.send(&Req::get("/none")).expect("send");

    assert_eq!(response.status(), 204);
    assert!(response.body().is_empty());
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_binary_body_is_returned_unchanged() {
    let payload = [0x00, 0xff, 0x01, 0xfe, 0x00];
    let (_server, client) = serve(|then| {
        then.status(200).body(payload);
    });

    let response = client.send(&Req::get("/bin")).expect("send");

    assert_eq!(response.body(), payload);
}

#[test]
#[cfg_attr(miri, ignore)]
fn header_get_is_case_insensitive_and_empty_when_absent() {
    let (_server, client) = serve(|then| {
        then.status(200)
            .header("Datadog-Container-Tags-Hash", "abc123")
            .header("ETag", "\"v1\"");
    });

    let response = client.send(&Req::get("/info")).expect("send");

    for name in [
        "Datadog-Container-Tags-Hash",
        "datadog-container-tags-hash",
        "DATADOG-CONTAINER-TAGS-HASH",
    ] {
        assert_eq!(response.header(name).as_deref(), Some("abc123"), "{name}");
    }
    assert_eq!(response.header("etag").as_deref(), Some("\"v1\""));
    assert_eq!(response.header("x-not-there"), None);
}

#[test]
#[cfg_attr(miri, ignore)]
fn headers_returns_every_header_and_keeps_repeated_names() {
    let (_server, client) = serve(|then| {
        then.status(200)
            .header("Set-Cookie", "a=1")
            .header("X-Between", "b")
            .header("Set-Cookie", "c=3");
    });

    let response = client.send(&Req::get("/cookies")).expect("send");

    let headers = response.headers();
    let cookies: Vec<&str> = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
        .map(|(_, value)| value.as_str())
        .collect();
    assert_eq!(cookies, vec!["a=1", "c=3"]);
    assert_eq!(response.header("set-cookie").as_deref(), Some("a=1"));
    // Headers of different names are not guaranteed to be in the order received.
    assert!(
        headers
            .iter()
            .any(|(n, v)| n.eq_ignore_ascii_case("x-between") && v == "b")
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn headers_returns_the_same_slice_on_every_call() {
    let (_server, client) = serve(|then| {
        then.status(200).header("X-First", "1");
    });
    let response = client.send(&Req::get("/h")).expect("send");

    let first = unsafe { ddog_http_response_headers(response.0) };
    let second = unsafe { ddog_http_response_headers(response.0) };

    assert!(!first.is_empty());
    assert_eq!(first.as_raw_parts(), second.as_raw_parts());
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_response_stays_valid_after_its_client_is_dropped() {
    let (_server, client) = serve(|then| {
        then.status(200).header("X-Kept", "yes").body("still here");
    });
    let response = client.send(&Req::get("/keep")).expect("send");

    drop(client);

    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), b"still here");
    assert_eq!(response.header("x-kept").as_deref(), Some("yes"));
}
