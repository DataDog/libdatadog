// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(unix)]
use criterion::{criterion_group, criterion_main, Criterion};
#[cfg(unix)]
use datadog_ipc::example_interface::{ExampleInterfaceChannel, ExampleServer};

#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use tokio::runtime;

#[cfg(unix)]
fn criterion_benchmark(c: &mut Criterion) {
    let (conn_server, conn_client) = datadog_ipc::SeqpacketConn::socketpair().unwrap();

    let worker = thread::spawn(move || {
        let rt = runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let server = ExampleServer::default();
        rt.block_on(server.accept_connection(conn_server));
    });

    let mut channel = ExampleInterfaceChannel::new(conn_client);

    c.bench_function("write only interface", |b| {
        b.iter(|| channel.try_send_notify())
    });

    // This consistently blocks on aarch64 (both MacOS and Linux), is there an issue with the
    // optimized code?
    #[cfg(not(target_arch = "aarch64"))]
    c.bench_function("two way interface", |b| {
        b.iter(|| channel.call_req_cnt().unwrap())
    });

    #[cfg(not(target_arch = "aarch64"))]
    println!(
        "Total requests handled: {}",
        channel.call_req_cnt().unwrap()
    );

    drop(channel);
    worker.join().unwrap();
}

#[cfg(unix)]
criterion_group!(benches, criterion_benchmark);
#[cfg(unix)]
criterion_main!(benches);

#[cfg(windows)]
fn main() {
    println!("IPC benches not implemented for Windows")
}
