// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::*;
use httpmock::prelude::*;
use libdd_http_client_ffi::*;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const DELAY: Duration = Duration::from_secs(3);

/// A server whose every answer arrives after `DELAY`.
fn slow_server() -> MockServer {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.path_prefix("/");
        then.status(200).delay(DELAY);
    });
    server
}

/// Closes the first `dropped` connections without answering, then answers every other one.
fn flaky_server(dropped: usize) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    thread::spawn(move || {
        for (index, stream) in listener.incoming().flatten().enumerate() {
            let mut stream = stream;
            let _ = stream.read(&mut [0u8; 4096]);
            if index >= dropped {
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecond",
                );
            }
        }
    });
    addr
}

#[test]
fn a_refused_connection_is_connection_failed() {
    let client = Cfg::for_addr(refused_addr()).build().expect("client");

    let error = client.send(&Req::get("/info")).err().expect("must fail");

    assert_eq!(
        error.code,
        HttpClientErrorCode::ConnectionFailed,
        "{}",
        error.message
    );
}

#[test]
fn a_silent_server_times_out_using_the_client_timeout() {
    let server = slow_server();
    let client = Cfg::new(server.base_url())
        .timeout_ms(300)
        .build()
        .expect("client");

    let started = Instant::now();
    let error = client
        .send(&Req::get("/slow"))
        .err()
        .expect("must time out");

    assert_eq!(
        error.code,
        HttpClientErrorCode::TimedOut,
        "{}",
        error.message
    );
    assert!(started.elapsed() < DELAY, "took {:?}", started.elapsed());
}

#[test]
fn a_request_timeout_overrides_the_client_timeout() {
    let server = slow_server();
    let client = Cfg::new(server.base_url())
        .timeout_ms(60_000)
        .build()
        .expect("client");

    let started = Instant::now();
    let error = client
        .send(&Req::get("/slow").timeout_ms(300))
        .err()
        .expect("must time out");

    assert_eq!(
        error.code,
        HttpClientErrorCode::TimedOut,
        "{}",
        error.message
    );
    assert!(started.elapsed() < DELAY, "took {:?}", started.elapsed());
}

#[test]
fn a_dropped_connection_is_an_error_without_retries() {
    let client = Cfg::for_addr(flaky_server(1)).build().expect("client");

    assert!(client.send(&Req::get("/flaky")).is_err());
}

#[test]
fn retries_recover_from_a_dropped_connection() {
    let client = Cfg::for_addr(flaky_server(1))
        .retries(2, 10)
        .build()
        .expect("client");

    let response = client
        .send(&Req::get("/flaky"))
        .expect("retry must succeed");

    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), b"second");
}

#[test]
fn an_http_error_status_is_never_retried() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.path("/once");
        then.status(500).body("no");
    });
    let client = Cfg::new(server.base_url())
        .retries(3, 10)
        .build()
        .expect("client");

    let response = client
        .send(&Req::get("/once"))
        .expect("a status is not an error");

    assert_eq!(response.status(), 500);
    mock.assert();
}

#[test]
fn cancelling_from_another_thread_aborts_a_blocked_send() {
    let server = slow_server();
    let client = Cfg::new(server.base_url())
        .timeout_ms(60_000)
        .build()
        .expect("client");
    // Shared through `Arc` so the token outlives both the send and the cancel.
    let token = Arc::new(Token::new());
    let canceller_token = token.clone();

    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        assert!(canceller_token.cancel());
    });
    let started = Instant::now();
    let error = client
        .send_with(&Req::get("/slow"), Some(&token))
        .err()
        .expect("must be cancelled");
    canceller.join().expect("canceller panicked");

    assert_eq!(
        error.code,
        HttpClientErrorCode::Cancelled,
        "{}",
        error.message
    );
    assert!(started.elapsed() < DELAY, "took {:?}", started.elapsed());
}

#[test]
fn an_already_cancelled_token_fails_without_sending() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.path_prefix("/");
        then.status(200);
    });
    let client = Cfg::new(server.base_url()).build().expect("client");
    let token = Token::new();
    assert!(token.cancel());

    let error = client
        .send_with(&Req::get("/never"), Some(&token))
        .err()
        .expect("must be cancelled");

    assert_eq!(error.code, HttpClientErrorCode::Cancelled);
    mock.assert_calls(0);
}

#[test]
fn an_uncancelled_token_does_not_disturb_a_send() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.path_prefix("/");
        then.status(200);
    });
    let client = Cfg::new(server.base_url()).build().expect("client");
    let token = Token::new();

    let response = client
        .send_with(&Req::get("/fine"), Some(&token))
        .expect("send");

    assert_eq!(response.status(), 200);
}

#[test]
fn the_token_is_still_usable_after_a_cancelled_send() {
    let client = Cfg::new("http://localhost").build().expect("client");
    let token = Token::new();
    token.cancel();

    let first = client.send_with(&Req::get("/a"), Some(&token));
    let second = client.send_with(&Req::get("/b"), Some(&token));

    assert_eq!(
        first.err().expect("first").code,
        HttpClientErrorCode::Cancelled
    );
    assert_eq!(
        second.err().expect("second").code,
        HttpClientErrorCode::Cancelled
    );
}
