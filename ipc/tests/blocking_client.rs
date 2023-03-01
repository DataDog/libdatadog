// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
#![cfg(unix)]
use std::{
    io::Write,
    os::unix::net::UnixStream as StdUnixStream,
    thread::{self},
    time::{Duration, Instant},
};

use tokio::{net::UnixStream, runtime};

use datadog_ipc::example_interface::{
    ExampleInterfaceRequest, ExampleInterfaceResponse, ExampleServer, ExampleTransport,
};

#[test]
fn test_blocking_client() {
    let (sock_a, sock_b) = StdUnixStream::pair().unwrap();

    let worker = thread::spawn(move || {
        let rt = runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _g = rt.enter();
        sock_a.set_nonblocking(true).unwrap();
        let socket = UnixStream::from_std(sock_a).unwrap();
        let server = ExampleServer::default();

        rt.block_on(server.accept_connection(socket));
    });

    let mut transport = ExampleTransport::from(sock_b);
    transport.set_nonblocking(true).unwrap(); // sending one-way messages should be instantaineous, even if the RPC worker is not fully up
    transport.send(ExampleInterfaceRequest::Ping {}).unwrap();
    transport.set_nonblocking(false).unwrap(); // write should still be quick, but we'll have to block waiting for RPC worker to come up

    transport
        .set_write_timeout(Some(Duration::from_nanos(1)))
        .unwrap();
    match transport.call(ExampleInterfaceRequest::TimeNow {}).unwrap() {
        ExampleInterfaceResponse::TimeNow(time) => {
            assert!(Instant::now().elapsed().saturating_sub(time) < Duration::from_millis(10));
        }
        _ => panic!("shouldn't happen"),
    }

    transport
        .set_read_timeout(Some(Duration::from_millis(3)))
        .unwrap(); // the RPC worker is up at this point - the read should be very quick

    match transport.call(ExampleInterfaceRequest::ReqCnt {}).unwrap() {
        ExampleInterfaceResponse::ReqCnt(cnt) => assert_eq!(2, cnt),
        _ => panic!("shouldn't happen"),
    }

    let f = tempfile::tempfile().unwrap();
    transport
        .call(ExampleInterfaceRequest::StoreFile { file: f.into() })
        .unwrap();

    let f = match transport
        .call(ExampleInterfaceRequest::RetrieveFile {})
        .unwrap()
    {
        ExampleInterfaceResponse::RetrieveFile(f) => f.unwrap(),
        _ => panic!("shouldn't happen"),
    };

    let mut f = f.into_instance().unwrap();
    writeln!(f, "test").unwrap(); // file should still be writeable

    drop(transport);
    worker.join().unwrap();
}
