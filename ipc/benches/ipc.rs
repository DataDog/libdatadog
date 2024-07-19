// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(not(windows))]
use criterion::{criterion_group, criterion_main, Criterion};
#[cfg(all(not(windows), not(target_arch = "aarch64")))]
use datadog_ipc::example_interface::ExampleInterfaceResponse;
#[cfg(not(windows))]
use datadog_ipc::{
    example_interface::{ExampleInterfaceRequest, ExampleServer, ExampleTransport},
    platform::Channel,
};

#[cfg(not(windows))]
use std::{
    os::unix::net::UnixStream,
    thread::{self},
};
#[cfg(not(windows))]
use tokio::runtime;

#[cfg(not(windows))]
fn criterion_benchmark(c: &mut Criterion) {
    let (sock_a, sock_b) = UnixStream::pair().unwrap();

    let worker = thread::spawn(move || {
        let rt = runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _g = rt.enter();
        sock_a.set_nonblocking(true).unwrap();
        let server = ExampleServer::default();

        rt.block_on(server.accept_connection(Channel::from(sock_a)));
    });

    let mut transport = ExampleTransport::from(sock_b);
    transport.set_nonblocking(false).unwrap();

    c.bench_function("write only interface", |b| {
        b.iter(|| transport.send(ExampleInterfaceRequest::Notify {}).unwrap())
    });

    // This consistently blocks on aarch64 (both MacOS and Linux), is there an issue with the
    // optimized code?
    #[cfg(not(target_arch = "aarch64"))]
    c.bench_function("two way interface", |b| {
        b.iter(|| transport.call(ExampleInterfaceRequest::ReqCnt {}).unwrap())
    });

    // This consistently blocks on aarch64 (both MacOS and Linux), is there an issue with the
    // optimized code?
    #[cfg(not(target_arch = "aarch64"))]
    match transport.call(ExampleInterfaceRequest::ReqCnt {}).unwrap() {
        ExampleInterfaceResponse::ReqCnt(cnt) => {
            println!("Total requests handled: {cnt}");
        }
        _ => panic!("shouldn't happen"),
    };

    drop(transport);
    worker.join().unwrap();
}

#[cfg(not(windows))]
criterion_group!(benches, criterion_benchmark);

#[cfg(not(windows))]
criterion_main!(benches);

#[cfg(windows)]
fn main() {
    println!("IPC benches not implemented for Windows")
}
