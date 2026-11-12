// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::*;
use libdd_common_ffi::{CharSlice, Slice};
use libdd_http_client_ffi::*;
use std::ptr::{NonNull, null, null_mut};

fn expect_error(result: Result<Client, FfiError>) -> FfiError {
    match result {
        Ok(_) => panic!("expected an error, got a client"),
        Err(error) => error,
    }
}

fn config_for<'a>(
    base_url: CharSlice<'a>,
    timeout_ms: u64,
    headers: &'a [HttpHeader<'a>],
) -> HttpClientConfig<'a> {
    HttpClientConfig {
        base_url,
        timeout_ms,
        unix_socket_path: CharSlice::default(),
        default_headers: Slice::from(headers),
        periodic: false,
        max_retries: 0,
        retry_initial_delay_ms: 0,
    }
}

#[test]
fn new_succeeds_for_a_tcp_config_without_connecting() {
    // Nothing listens here: building a client must not connect.
    let client = Cfg::for_addr(refused_addr()).build();
    assert!(client.is_ok(), "{:?}", client.err());
}

#[test]
fn new_accepts_every_optional_setting() {
    let mut cfg = Cfg::new("http://localhost:8126")
        .default_header("Datadog-Meta-Lang", "ruby")
        .retries(3, 50);
    cfg.periodic = true;
    let client = cfg.build();
    assert!(client.is_ok(), "{:?}", client.err());
}

// Not run on reqwest, which panics without an installed crypto provider.
#[cfg(all(feature = "https", not(feature = "reqwest-backend")))]
#[test]
fn new_succeeds_for_an_https_base_url_without_a_preinstalled_crypto_provider() {
    let client = Cfg::new("https://localhost:8443").build();
    assert!(client.is_ok(), "{:?}", client.err());
}

#[test]
fn new_rejects_a_null_config() {
    let mut out = null_mut();
    let error = take_error(unsafe { ddog_http_client_new(null(), null(), &mut out) })
        .expect_err("a null config must be rejected");
    assert_eq!(error.code, HttpClientErrorCode::InvalidArgument);
}

#[test]
fn new_rejects_a_null_out_pointer() {
    let config = config_for(CharSlice::from("http://localhost"), 1_000, &[]);
    let error = take_error(unsafe { ddog_http_client_new(&config, null(), null_mut()) })
        .expect_err("a null out pointer must be rejected");
    assert_eq!(error.code, HttpClientErrorCode::InvalidArgument);
}

#[test]
fn new_rejects_an_empty_base_url() {
    let error = expect_error(Cfg::new("").build());
    assert_eq!(error.code, HttpClientErrorCode::InvalidConfig);
}

#[test]
fn new_rejects_a_zero_timeout() {
    let error = expect_error(Cfg::new("http://localhost").timeout_ms(0).build());
    assert_eq!(error.code, HttpClientErrorCode::InvalidConfig);
}

#[test]
fn new_rejects_a_base_url_that_is_not_a_url() {
    let error = expect_error(Cfg::new("not a url").build());
    assert_eq!(error.code, HttpClientErrorCode::InvalidConfig);
}

#[test]
fn new_rejects_an_unsupported_scheme() {
    let error = expect_error(Cfg::new("ftp://localhost").build());
    assert_eq!(error.code, HttpClientErrorCode::InvalidConfig);
}

#[test]
fn new_rejects_a_base_url_that_is_not_utf8() {
    let invalid = [b'h', b't', b't', b'p', b':', b'/', b'/', 0xff, 0xfe];
    let config = config_for(CharSlice::from_bytes(&invalid), 1_000, &[]);
    let mut out = null_mut();
    let error = take_error(unsafe { ddog_http_client_new(&config, null(), &mut out) })
        .expect_err("a non-UTF-8 base_url must be rejected");
    assert_eq!(error.code, HttpClientErrorCode::InvalidArgument);
}

#[test]
fn new_leaves_the_out_pointer_untouched_on_error() {
    let config = config_for(CharSlice::from(""), 1_000, &[]);
    let sentinel: *mut HttpClient = NonNull::dangling().as_ptr();
    let mut out = sentinel;
    let error = take_error(unsafe { ddog_http_client_new(&config, null(), &mut out) });
    assert!(error.is_err());
    assert_eq!(
        out, sentinel,
        "*out must not be written when an error is returned"
    );
}

#[test]
fn drop_accepts_null() {
    unsafe { ddog_http_client_drop(None) };
}

#[test]
fn freeing_a_null_error_is_a_no_op() {
    unsafe { ddog_http_client_error_free(None) };
}

#[test]
fn error_message_is_not_empty() {
    let error = expect_error(Cfg::new("").build());
    assert!(!error.message.is_empty());
}

#[test]
fn a_client_with_a_shared_runtime_keeps_working_after_the_caller_frees_its_reference() {
    let server = spawn_tcp(respond_ok);
    let runtime = new_shared_runtime();
    let client = server.cfg().build_with_runtime(runtime).expect("client");
    unsafe { free_shared_runtime(runtime) };

    let response = client.send(&Req::get("/ping")).expect("send");
    assert_eq!(response.status(), 200);
}

#[test]
fn several_clients_can_share_one_runtime() {
    let server = spawn_tcp(respond_ok);
    let runtime = new_shared_runtime();
    let first = server
        .cfg()
        .build_with_runtime(runtime)
        .expect("first client");
    let second = server
        .cfg()
        .build_with_runtime(runtime)
        .expect("second client");
    unsafe { free_shared_runtime(runtime) };

    assert_eq!(first.send(&Req::get("/a")).expect("first").status(), 200);
    drop(first);
    // The second client must survive the first being dropped.
    assert_eq!(second.send(&Req::get("/b")).expect("second").status(), 200);
}
