// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]
use std::time::{Duration, Instant};

use tokio::runtime;

use datadog_ipc::example_interface::{ExampleInterfaceChannel, ExampleServer};
use datadog_ipc::SeqpacketConn;

#[test]
#[cfg_attr(miri, ignore)]
fn test_blocking_client() {
    let (conn_server, conn_client) = SeqpacketConn::socketpair().unwrap();

    // Setup async server
    let rt = runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    rt.spawn({
        let server = ExampleServer::default();
        async move { server.accept_connection(conn_server).await }
    });

    let mut channel = ExampleInterfaceChannel::new(conn_client);

    // Fire-and-forget ping (blocking variant that waits for ack)
    channel.call_ping().unwrap();

    // Blocking call with response
    let time = channel.call_time_now().unwrap();
    assert!(Instant::now().elapsed().saturating_sub(time) < Duration::from_millis(10));

    // req_cnt should be 2 (ping + time_now)
    assert_eq!(2, channel.call_req_cnt().unwrap());

    // Store a file via handle transfer
    let f = tempfile::tempfile().unwrap();
    channel.try_send_store_file(f.into());
}
