// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Requests that do not produce a response: refused connections, timeouts, retries and
//! cancellation.

mod common;

use common::*;
use libdd_http_client_ffi::*;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

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
    let server = spawn_tcp(|_| Behavior::Hang);
    let client = server.cfg().timeout_ms(300).build().expect("client");

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
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_request_timeout_overrides_the_client_timeout() {
    let server = spawn_tcp(|_| Behavior::Hang);
    let client = server.cfg().timeout_ms(60_000).build().expect("client");

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
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_dropped_connection_is_an_error_without_retries() {
    let server = spawn_tcp(|_| Behavior::CloseImmediately);
    let client = server.cfg().build().expect("client");

    assert!(client.send(&Req::get("/flaky")).is_err());
}

#[test]
fn retries_recover_from_a_dropped_connection() {
    // The first connection is closed without an answer; the retry succeeds.
    let server = spawn_tcp(|connection| {
        if connection == 0 {
            Behavior::CloseImmediately
        } else {
            Behavior::Respond(http_response(200, &[], b"second try"))
        }
    });
    let client = server.cfg().retries(2, 10).build().expect("client");

    let response = client
        .send(&Req::get("/flaky"))
        .expect("retry must succeed");

    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), b"second try");
    assert_eq!(server.recv().target, "/flaky");
    assert_eq!(server.recv().target, "/flaky");
}

#[test]
fn an_http_error_status_is_never_retried() {
    let server = spawn_tcp(|_| Behavior::Respond(http_response(500, &[], b"no")));
    let client = server.cfg().retries(3, 10).build().expect("client");

    let response = client
        .send(&Req::get("/once"))
        .expect("a status is not an error");

    assert_eq!(response.status(), 500);
    server.recv();
    assert!(
        server.nothing_within(Duration::from_millis(300)),
        "a status must not be retried"
    );
}

#[test]
fn cancelling_from_another_thread_aborts_a_blocked_send() {
    let server = spawn_tcp(|_| Behavior::Hang);
    // The client timeout is far longer than the test: only the cancellation can end the send.
    let client = server.cfg().timeout_ms(60_000).build().expect("client");
    // One token shared by both threads. Each holds an `Arc`, so the token outlives the send and
    // the `cancel` call, which is what the API requires.
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
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn an_already_cancelled_token_fails_without_sending() {
    let server = spawn_tcp(respond_ok);
    let client = server.cfg().build().expect("client");
    let token = Token::new();
    assert!(token.cancel());

    let error = client
        .send_with(&Req::get("/never"), Some(&token))
        .err()
        .expect("must be cancelled");

    assert_eq!(error.code, HttpClientErrorCode::Cancelled);
    assert!(
        server.nothing_within(Duration::from_millis(300)),
        "nothing may be sent"
    );
}

#[test]
fn an_uncancelled_token_does_not_disturb_a_send() {
    let server = spawn_tcp(respond_ok);
    let client = server.cfg().build().expect("client");
    let token = Token::new();

    let response = client
        .send_with(&Req::get("/fine"), Some(&token))
        .expect("send");

    assert_eq!(response.status(), 200);
}

#[test]
fn the_token_is_still_usable_after_a_cancelled_send() {
    let server = spawn_tcp(respond_ok);
    let client = server.cfg().build().expect("client");
    let token = Token::new();
    token.cancel();

    let first = client.send_with(&Req::get("/a"), Some(&token));
    let second = client.send_with(&Req::get("/b"), Some(&token));

    // Cancellation is terminal: the same token cancels every later send too.
    assert_eq!(
        first.err().expect("first").code,
        HttpClientErrorCode::Cancelled
    );
    assert_eq!(
        second.err().expect("second").code,
        HttpClientErrorCode::Cancelled
    );
}

#[test]
fn cancelling_is_idempotent_and_reports_only_the_first_call() {
    let token = Token::new();

    assert!(token.cancel());
    assert!(!token.cancel());
    assert!(!token.cancel());
}

#[test]
fn dropping_a_null_token_is_a_no_op() {
    // SAFETY: `None` (NULL) is part of the documented contract of `drop`.
    unsafe { ddog_http_cancel_token_drop(None) };
}
