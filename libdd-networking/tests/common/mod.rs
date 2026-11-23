// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Helpers shared by the client integration tests.

#![allow(dead_code)]

extern crate alloc;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::task::{Context, Poll};

use httpmock::prelude::*;
use libdd_networking::connector::Connector;
use libdd_networking::http_common::{self, Body, GenericHttpClient};

/// Start a mock server that answers `ok` to `GET /`.
pub fn start_server() -> MockServer {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/");
        then.status(200).body("ok");
    });
    server
}

/// Send a GET request and check the server answered `ok`.
pub async fn get_ok<C: libdd_networking::Connect>(client: &GenericHttpClient<C>, url: &str) {
    let req = http::Request::get(url).body(Body::empty()).unwrap();
    let resp = client.request(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    let bytes = http_common::collect_response_bytes(http_common::into_response(resp))
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"ok");
}

/// [`Connector`] that counts the connections it opens.
#[derive(Clone, Default)]
pub struct CountingConnector {
    inner: Connector,
    connects: Arc<AtomicUsize>,
}

impl CountingConnector {
    /// Number of connections opened so far.
    pub fn connects(&self) -> usize {
        self.connects.load(Ordering::SeqCst)
    }
}

impl tower_service::Service<http::Uri> for CountingConnector {
    type Response = <Connector as tower_service::Service<http::Uri>>::Response;
    type Error = <Connector as tower_service::Service<http::Uri>>::Error;
    type Future = <Connector as tower_service::Service<http::Uri>>::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, uri: http::Uri) -> Self::Future {
        self.connects.fetch_add(1, Ordering::SeqCst);
        self.inner.call(uri)
    }
}
