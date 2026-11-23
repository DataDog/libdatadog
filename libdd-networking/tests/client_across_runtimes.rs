// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Clients from `http_common` keep pooling connections and keep working across runtimes, with or
//! without the `weak-waker` feature.

#![cfg(feature = "http-client")]

mod common;

use common::{CountingConnector, get_ok, start_server};
use libdd_networking::http_common;

fn build_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn pooled_connection_is_reused() {
    let server = start_server();
    let url = server.url("/");
    let connector = CountingConnector::default();
    let mut builder = http_common::client_builder();
    builder.pool_max_idle_per_host(20);
    let client = builder.build(connector.clone());
    let rt = build_runtime();
    rt.block_on(async {
        get_ok(&client, &url).await;
        get_ok(&client, &url).await;
    });
    assert_eq!(connector.connects(), 1);
}

#[test]
fn client_works_on_a_new_runtime_after_the_old_one_drops() {
    let server = start_server();
    let url = server.url("/");
    for client in [
        http_common::new_client_periodic(),
        http_common::new_default_client(),
    ] {
        let rt = build_runtime();
        rt.block_on(get_ok(&client, &url));
        drop(rt);

        let rt = build_runtime();
        rt.block_on(get_ok(&client, &url));
    }
}
