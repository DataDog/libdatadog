// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Clients built with `WeakWakerExecutor` keep pooling connections and keep working across
//! runtimes.

#![cfg(feature = "http-client")]

mod common;

use common::{KeepAliveServer, get_ok};
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
    let server = KeepAliveServer::start();
    let client = http_common::new_client_periodic();
    let rt = build_runtime();
    rt.block_on(async {
        get_ok(&client, server.addr).await;
        get_ok(&client, server.addr).await;
    });
    assert_eq!(server.accepted(), 1);
}

#[test]
fn client_works_on_a_new_runtime_after_the_old_one_drops() {
    let server = KeepAliveServer::start();
    for client in [
        http_common::new_client_periodic(),
        http_common::new_default_client(),
    ] {
        let rt = build_runtime();
        rt.block_on(get_ok(&client, server.addr));
        drop(rt);

        let rt = build_runtime();
        rt.block_on(get_ok(&client, server.addr));
    }
}
