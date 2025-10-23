// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]
use std::{
    io::Write,
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

use tokio::runtime;

use libdd_ipc::example_interface::{
    ExampleInterfaceRequest, ExampleInterfaceResponse, ExampleServer, ExampleTransport,
};
use libdd_ipc::platform::Channel;

#[test]
#[cfg_attr(miri, ignore)]
fn test_blocking_client() {
    let (sock_a, sock_b) = UnixStream::pair().unwrap();
    // Setup async server
    let rt = runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    {
        // drop guard at the end of the code
        let _g = rt.enter();
        sock_a.set_nonblocking(true).unwrap();

        let socket = Channel::from(sock_a);
        let server = ExampleServer::default();

        rt.spawn(server.accept_connection(socket));
    }

    // Test blocking sync code
    let mut transport = ExampleTransport::from(sock_b);
    transport.set_nonblocking(true).unwrap(); // sending one-way messages should be instantaineous, even if the RPC worker is not fully up
    transport.send(&ExampleInterfaceRequest::Ping {}).unwrap();
    transport.set_nonblocking(false).unwrap(); // write should still be quick, but we'll have to block waiting for RPC worker to come up

    transport
        .set_write_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    match transport
        .call(&ExampleInterfaceRequest::TimeNow {})
        .unwrap()
    {
        ExampleInterfaceResponse::TimeNow(time) => {
            assert!(Instant::now().elapsed().saturating_sub(time) < Duration::from_millis(10));
        }
        _ => panic!("shouldn't happen"),
    }

    transport
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap(); // the RPC worker is up at this point - the read should be very quick

    match transport.call(&ExampleInterfaceRequest::ReqCnt {}).unwrap() {
        ExampleInterfaceResponse::ReqCnt(cnt) => assert_eq!(2, cnt),
        _ => panic!("shouldn't happen"),
    }

    let f = tempfile::tempfile().unwrap();
    transport
        .call(&ExampleInterfaceRequest::StoreFile { file: f.into() })
        .unwrap();

    let f = match transport
        .call(&ExampleInterfaceRequest::RetrieveFile {})
        .unwrap()
    {
        ExampleInterfaceResponse::RetrieveFile(f) => f.unwrap(),
        _ => panic!("shouldn't happen"),
    };
    let mut f = f.into_instance().unwrap();
    writeln!(f, "test").unwrap(); // file should still be writeable
}
