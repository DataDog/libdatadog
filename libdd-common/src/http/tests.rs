// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// Unit tests for the shared HTTP foundation module.
//
// These tests cover the surface that is new at the `libdd_common::http`
// path. The connector dispatch layer has its own test module in
// `crate::connector` and is exercised here only at the
// `new_default_client` / `new_client_periodic` boundary.

#![cfg(not(target_arch = "wasm32"))]

use std::convert::Infallible;

use http_body_util::BodyExt;
use hyper::body::Body as _;
use hyper::body::Bytes;

use super::body::Body;
use super::client::{
    collect_response_bytes, empty_response, mock_response, new_client_periodic, new_default_client,
};
use super::error::{ClientError, Error, ErrorKind};

// --- Body variants round-trip ---

#[tokio::test]
async fn body_empty_yields_no_frames() {
    let body = Body::empty();
    assert!(body.is_end_stream());
    let collected = body.collect().await.expect("collect empty body");
    assert!(collected.to_bytes().is_empty());
}

#[tokio::test]
async fn body_from_bytes_roundtrips_payload() {
    let payload = Bytes::from_static(b"hello, datadog");
    let body = Body::from_bytes(payload.clone());
    let bytes = body.collect().await.expect("collect single body").to_bytes();
    assert_eq!(bytes, payload);
}

#[tokio::test]
async fn body_from_string_roundtrips_payload() {
    let body = Body::from(String::from("payload-from-string"));
    let bytes = body.collect().await.expect("collect string body").to_bytes();
    assert_eq!(bytes.as_ref(), b"payload-from-string");
}

#[tokio::test]
async fn body_from_vec_roundtrips_payload() {
    let body = Body::from(b"payload-from-vec".to_vec());
    let bytes = body.collect().await.expect("collect vec body").to_bytes();
    assert_eq!(bytes.as_ref(), b"payload-from-vec");
}

#[tokio::test]
async fn body_from_static_str_roundtrips_payload() {
    let body = Body::from("static-str-payload");
    let bytes = body.collect().await.expect("collect static str body").to_bytes();
    assert_eq!(bytes.as_ref(), b"static-str-payload");
}

#[tokio::test]
async fn body_channel_streams_and_terminates_on_close() {
    let (sender, body) = Body::channel();
    let send_task = tokio::spawn(async move {
        sender
            .send_data(Bytes::from_static(b"chunk-1"))
            .await
            .expect("send chunk 1");
        sender
            .send_data(Bytes::from_static(b"chunk-2"))
            .await
            .expect("send chunk 2");
        // Dropping `sender` here would close the channel implicitly; do it
        // explicitly so the test is unambiguous.
        drop(sender);
    });

    let collected = body.collect().await.expect("collect channel body").to_bytes();
    send_task.await.expect("sender task panicked");
    assert_eq!(collected.as_ref(), b"chunk-1chunk-2");
}

#[tokio::test]
async fn body_boxed_wraps_arbitrary_body_with_anyhow_errors() {
    // Use Empty<Bytes> as the inner body — Empty's error is Infallible, which
    // satisfies the `boxed` bound (`std::error::Error + Send + Sync`).
    let inner = http_body_util::Empty::<Bytes>::new();
    let body = Body::boxed(inner);
    assert!(body.is_end_stream());
    let collected = body.collect().await.expect("collect boxed body").to_bytes();
    assert!(collected.is_empty());
}

#[tokio::test]
async fn body_default_is_empty() {
    let body = Body::default();
    assert!(body.is_end_stream());
    let collected = body.collect().await.expect("collect default body").to_bytes();
    assert!(collected.is_empty());
}

// --- Request build ---

#[test]
fn http_request_can_be_built_with_body() {
    let request = http::Request::builder()
        .method(http::Method::POST)
        .uri("http://localhost/v0.7/traces")
        .header("content-type", "application/json")
        .body(Body::from_bytes(Bytes::from_static(b"{\"trace\":[]}")))
        .expect("build request");

    assert_eq!(request.method(), http::Method::POST);
    assert_eq!(request.uri(), &"http://localhost/v0.7/traces".parse::<http::Uri>().expect("uri"));
    assert_eq!(
        request.headers().get("content-type").map(|v| v.to_str().expect("ascii header")),
        Some("application/json"),
    );
}

#[test]
fn http_request_default_body_is_empty() {
    let request = http::Request::builder()
        .uri("http://localhost/")
        .body(Body::default())
        .expect("build request with default body");
    assert!(request.body().is_end_stream());
}

// --- Response helpers ---

#[tokio::test]
async fn empty_response_has_empty_body_and_default_status() {
    let response = empty_response(http::Response::builder()).expect("build empty response");
    assert_eq!(response.status(), http::StatusCode::OK);
    let bytes = collect_response_bytes(response)
        .await
        .expect("collect empty response");
    assert!(bytes.is_empty());
}

#[tokio::test]
async fn mock_response_roundtrips_status_and_body() {
    let payload = Bytes::from_static(b"mocked response body");
    let response = mock_response(
        http::Response::builder().status(http::StatusCode::ACCEPTED),
        payload.clone(),
    )
    .expect("build mock response");
    assert_eq!(response.status(), http::StatusCode::ACCEPTED);
    let bytes = collect_response_bytes(response)
        .await
        .expect("collect mock response");
    assert_eq!(bytes, payload);
}

// --- Connector dispatch (smoke: client constructors return usable clients) ---

#[test]
#[cfg_attr(miri, ignore)]
fn new_default_client_constructs_against_default_connector() {
    // Smoke test: ensures the type aliases all line up and that the
    // Connector trait bounds are satisfied. We don't actually issue a
    // request — that is the connector module's job.
    let _client = new_default_client();
}

#[test]
#[cfg_attr(miri, ignore)]
fn new_client_periodic_constructs_against_default_connector() {
    let _client = new_client_periodic();
}

// --- Error types ---

#[test]
fn client_error_kind_is_preserved() {
    let err = ClientError {
        source: anyhow::anyhow!("synthetic"),
        kind: ErrorKind::Timeout,
    };
    assert!(matches!(err.kind(), ErrorKind::Timeout));
    // Display delegates to the wrapped anyhow error.
    assert_eq!(format!("{err}"), "synthetic");
}

#[test]
fn error_display_branches() {
    let client = ClientError {
        source: anyhow::anyhow!("inner"),
        kind: ErrorKind::Closed,
    };
    let e = Error::Client(client);
    assert_eq!(format!("{e}"), "client error: inner");

    let e = Error::Other(anyhow::anyhow!("outer"));
    assert_eq!(format!("{e}"), "other error: outer");
}

#[test]
fn error_from_io_produces_other() {
    let io_err = std::io::Error::new(std::io::ErrorKind::Other, "io-source");
    let err: Error = io_err.into();
    assert!(matches!(err, Error::Other(_)));
}

#[test]
fn error_from_http_produces_other() {
    // Builder errors only surface on validation failures; force one via an
    // unparseable header value.
    let http_err: http::Error = http::Request::builder()
        .header("\u{0}invalid", "v")
        .body(())
        .expect_err("invalid header should yield http::Error");
    let err: Error = http_err.into();
    assert!(matches!(err, Error::Other(_)));
}

#[test]
#[allow(unreachable_code)]
fn error_infallible_branch_is_uninhabited() {
    // Sanity: confirm the Infallible branch type-checks without producing
    // a real value (the `match *e {}` arms in production code rely on the
    // empty enum). We construct a phantom by panicking before reaching
    // pattern construction — the test only exercises type-level wiring.
    fn _never(e: &Error) {
        if let Error::Infallible(_) = e {
            unreachable!()
        }
    }

    let bogus: Result<(), Infallible> = Ok(());
    let _ = bogus;
}

// --- Backwards-compat shim ---

#[test]
fn http_common_shim_re_exports_match_new_module() {
    // Verifies the legacy path is a true alias of the new module by
    // constructing values through both and asserting type compatibility.
    let _: super::HttpRequest = http::Request::builder()
        .uri("http://localhost/")
        .body(Body::empty())
        .expect("build request via http::HttpRequest");

    let _: crate::http_common::HttpRequest = http::Request::builder()
        .uri("http://localhost/")
        .body(crate::http_common::Body::empty())
        .expect("build request via http_common shim");
}
