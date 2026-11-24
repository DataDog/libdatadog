// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Check that a client with an idle pooled connection does not keep the tokio driver fds
//! (epoll and eventfd) open after the runtime drops, while the client lives on (APMSP-4334).
//!
//! Findings (hyper-util 0.1.17, Linux):
//! * the pool timer is not involved: a pending `TokioTimer` sleep alone does not leak;
//! * with `hyper_util::rt::TokioExecutor`, any client holding an idle pooled connection leaks until
//!   the client drops. The pooled sender shares the request channel with the connection task, and
//!   the channel keeps the task's tokio waker, which holds the scheduler handle;
//! * a client without pooling does not leak;
//! * with the `weak-waker` feature, the `http_common` constructors use `WeakWakerExecutor` and do
//!   not leak.
//!
//! The cases for the `http_common` constructors need the `weak-waker` feature. The leaking case
//! is `#[ignore]`d; run it with `--run-ignored all`.

#![cfg(all(target_os = "linux", feature = "http-client", not(miri)))]

mod common;

use core::time::Duration;
use std::sync::Mutex;
use std::time::Instant;

use common::{get_ok, start_server};
use httpmock::MockServer;
use libdd_networking::connector::Connector;
#[cfg(feature = "weak-waker")]
use libdd_networking::http_common;
use libdd_networking::http_common::GenericHttpClient;

/// fd counts are process-wide, so tests that read them must not overlap.
static FD_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    FD_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn driver_fds() -> (usize, usize) {
    let mut epoll = 0;
    let mut eventfd = 0;
    for entry in std::fs::read_dir("/proc/self/fd").unwrap().flatten() {
        if let Ok(target) = std::fs::read_link(entry.path()) {
            match target.to_string_lossy().as_ref() {
                "anon_inode:[eventpoll]" => epoll += 1,
                "anon_inode:[eventfd]" => eventfd += 1,
                _ => {}
            }
        }
    }
    (epoll, eventfd)
}

fn build_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap()
}

/// Send one request with `block_on`, then let the connection return to the pool.
fn send_block_on(
    rt: &tokio::runtime::Runtime,
    client: &GenericHttpClient<Connector>,
    server: &MockServer,
) {
    rt.block_on(async {
        get_ok(client, &server.url("/")).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    });
}

/// Send one request from a task spawned on the runtime, so the request runs with a tokio waker.
#[cfg(feature = "weak-waker")]
fn send_spawned(
    rt: &tokio::runtime::Runtime,
    client: &GenericHttpClient<Connector>,
    server: &MockServer,
) {
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let client = client.clone();
    let url = server.url("/");
    rt.spawn(async move {
        get_ok(&client, &url).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = done_tx.send(());
    });
    done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
}

fn wait_for_baseline(baseline: (usize, usize)) -> (usize, usize) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let now = driver_fds();
        if now == baseline || Instant::now() > deadline {
            return now;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

type Send = fn(&tokio::runtime::Runtime, &GenericHttpClient<Connector>, &MockServer);

/// Send with `send` on a fresh runtime, drop the runtime while keeping the client, and check
/// the driver fds are back to baseline.
fn run_case(client: GenericHttpClient<Connector>, send: Send) {
    let _guard = lock();
    let server = start_server();
    let baseline = driver_fds();
    let rt = build_runtime();
    eprintln!("baseline={baseline:?} with runtime={:?}", driver_fds());
    send(&rt, &client, &server);
    drop(rt);
    let after = wait_for_baseline(baseline);
    eprintln!("after runtime drop={after:?}");
    drop(client);
    let after_client = wait_for_baseline(baseline);
    eprintln!("after client drop={after_client:?}");
    assert_eq!(
        after, baseline,
        "tokio driver fds still open after runtime drop"
    );
}

fn tokio_executor_periodic_client() -> GenericHttpClient<Connector> {
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::default())
        .pool_timer(hyper_util::rt::TokioTimer::new())
        .pool_idle_timeout(Duration::from_secs(5))
        .pool_max_idle_per_host(20)
        .build(Connector::default())
}

#[test]
#[ignore = "leaks: with TokioExecutor, the idle pooled connection keeps the driver alive"]
fn diag_1_tokio_executor_leaks() {
    run_case(tokio_executor_periodic_client(), send_block_on);
}

#[cfg(feature = "weak-waker")]
#[test]
fn diag_2_default_client_releases() {
    run_case(http_common::new_default_client(), send_block_on);
}

#[test]
fn diag_4_no_pooling() {
    run_case(
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::default())
            .pool_max_idle_per_host(0)
            .build(Connector::default()),
        send_block_on,
    );
}

#[test]
fn diag_6_tokio_timer_alone() {
    let _guard = lock();
    let baseline = driver_fds();
    let rt = build_runtime();
    let timer = hyper_util::rt::TokioTimer::new();
    rt.spawn(async move {
        hyper::rt::Timer::sleep(&timer, Duration::from_secs(60)).await;
    });
    std::thread::sleep(Duration::from_millis(50));
    drop(rt);
    let after = wait_for_baseline(baseline);
    eprintln!("baseline={baseline:?} after runtime drop={after:?}");
    assert_eq!(after, baseline);
}

#[cfg(feature = "weak-waker")]
#[test]
fn diag_7_periodic_client_releases() {
    run_case(http_common::new_client_periodic(), send_block_on);
}

#[cfg(feature = "weak-waker")]
#[test]
fn diag_8_client_builder_releases() {
    let mut builder = http_common::client_builder();
    builder.pool_max_idle_per_host(20);
    run_case(builder.build(Connector::default()), send_block_on);
}

#[cfg(feature = "weak-waker")]
#[test]
fn diag_9_spawned_request_releases() {
    run_case(http_common::new_client_periodic(), send_spawned);
}

#[cfg(feature = "weak-waker")]
#[test]
fn diag_9_spawned_request_default_client_releases() {
    run_case(http_common::new_default_client(), send_spawned);
}

#[cfg(feature = "weak-waker")]
#[test]
fn diag_10_fork_like_cycle() {
    let _guard = lock();
    let server = start_server();
    let client = http_common::new_client_periodic();
    let baseline = driver_fds();
    for round in 0..2 {
        let rt = build_runtime();
        send_block_on(&rt, &client, &server);
        drop(rt);
        let after = wait_for_baseline(baseline);
        eprintln!("round {round}: baseline={baseline:?} after runtime drop={after:?}");
        assert_eq!(after, baseline, "round {round}: driver fds still open");
    }
}

#[test]
fn diag_0_no_request() {
    // Sanity check: a runtime alone releases its fds on drop.
    let _guard = lock();
    let baseline = driver_fds();
    let rt = build_runtime();
    rt.block_on(async { tokio::time::sleep(Duration::from_millis(10)).await });
    drop(rt);
    assert_eq!(wait_for_baseline(baseline), baseline);
}
