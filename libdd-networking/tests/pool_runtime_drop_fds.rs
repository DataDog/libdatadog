// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Diagnostic for APMSP-4334: does an idle pooled connection keep the tokio driver fds
//! (epoll/eventfd) open after the runtime is dropped, while the client lives on?
//!
//! Findings (hyper-util 0.1.17, Linux):
//! * a pending pool timer alone does not leak, with `TokioTimer` or with a weak timer;
//! * any client holding an idle pooled connection leaks, with `TokioTimer`, a weak timer, or no
//!   pool timer at all, until the client itself is dropped;
//! * a client without pooling does not leak.
//!
//! So the leak path is the pooled connection, not the pool timer. The leaking cases are
//! `#[ignore]`d; run them with `--ignored`.

#![cfg(all(target_os = "linux", feature = "http-client"))]

use core::net::SocketAddr;
use core::pin::Pin;
use core::task::{Context, Poll};
use core::time::Duration;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Mutex;
use std::time::Instant;

use libdd_networking::connector::Connector;
use libdd_networking::http_common::{self, Body, GenericHttpClient};

/// fd counts are process-wide, so tests that read them must not overlap.
static FD_LOCK: Mutex<()> = Mutex::new(());

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

fn spawn_keep_alive_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut writer = stream;
                loop {
                    let mut line = String::new();
                    // Read headers until the empty line.
                    loop {
                        line.clear();
                        match reader.read_line(&mut line) {
                            Ok(0) | Err(_) => return,
                            Ok(_) if line == "\r\n" => break,
                            Ok(_) => {}
                        }
                    }
                    if writer
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
    addr
}

fn build_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap()
}

fn send_one(rt: &tokio::runtime::Runtime, client: &GenericHttpClient<Connector>, addr: SocketAddr) {
    rt.block_on(async {
        let req = http::Request::get(format!("http://{addr}/"))
            .body(Body::empty())
            .unwrap();
        let resp = client.request(req).await.unwrap();
        assert_eq!(resp.status(), 200);
        let resp = http_common::into_response(resp);
        let bytes = http_common::collect_response_bytes(resp).await.unwrap();
        assert_eq!(&bytes[..], b"ok");
        // Let the connection go back to the pool and the idle task start.
        tokio::time::sleep(Duration::from_millis(50)).await;
    });
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

fn run_case(client: GenericHttpClient<Connector>) {
    let _guard = FD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let addr = spawn_keep_alive_server();
    let baseline = driver_fds();
    let rt = build_runtime();
    eprintln!("baseline={baseline:?} with runtime={:?}", driver_fds());
    send_one(&rt, &client, addr);
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

/// Test-local weak timer used to check the approach before the production version exists.
#[derive(Clone, Copy)]
struct DiagWeakTimer;

#[pin_project::pin_project]
struct DiagWeakSleep {
    deadline: tokio::time::Instant,
    #[pin]
    inner: Option<tokio::time::Sleep>,
}

impl core::future::Future for DiagWeakSleep {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut this = self.project();
        if this.inner.is_none() {
            this.inner
                .set(Some(tokio::time::sleep_until(*this.deadline)));
        }
        match this.inner.as_mut().as_pin_mut() {
            Some(s) => match s.poll(cx) {
                Poll::Ready(()) => {
                    this.inner.set(None);
                    Poll::Ready(())
                }
                Poll::Pending => Poll::Pending,
            },
            None => Poll::Ready(()),
        }
    }
}

impl hyper::rt::Sleep for DiagWeakSleep {}

impl hyper::rt::Timer for DiagWeakTimer {
    fn sleep(&self, d: Duration) -> Pin<Box<dyn hyper::rt::Sleep>> {
        self.sleep_until(Instant::now() + d)
    }
    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn hyper::rt::Sleep>> {
        Box::pin(DiagWeakSleep {
            deadline: tokio::time::Instant::from_std(deadline),
            inner: None,
        })
    }
}

fn periodic_with<T: hyper::rt::Timer + Clone + Send + Sync + 'static>(
    timer: T,
) -> GenericHttpClient<Connector> {
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::default())
        .pool_timer(timer)
        .pool_idle_timeout(Duration::from_secs(5))
        .pool_max_idle_per_host(20)
        .build(Connector::default())
}

#[test]
#[ignore = "leaks: the idle pooled connection keeps the driver alive"]
fn diag_1_periodic_tokio_timer() {
    run_case(periodic_with(hyper_util::rt::TokioTimer::new()));
}

#[test]
#[ignore = "leaks: the idle pooled connection keeps the driver alive"]
fn diag_2_default_client() {
    run_case(http_common::new_default_client());
}

#[test]
#[ignore = "leaks: the idle pooled connection keeps the driver alive"]
fn diag_3_periodic_weak_timer() {
    run_case(periodic_with(DiagWeakTimer));
}

#[test]
fn diag_4_no_pooling() {
    run_case(
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::default())
            .pool_max_idle_per_host(0)
            .build(Connector::default()),
    );
}

#[test]
fn diag_5_weak_timer_alone() {
    let _guard = FD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let baseline = driver_fds();
    let rt = build_runtime();
    let timer = DiagWeakTimer;
    rt.spawn(async move {
        hyper::rt::Timer::sleep(&timer, Duration::from_secs(60)).await;
    });
    std::thread::sleep(Duration::from_millis(50));
    drop(rt);
    let after = wait_for_baseline(baseline);
    eprintln!("baseline={baseline:?} after runtime drop={after:?}");
    assert_eq!(after, baseline);
}

#[test]
fn diag_6_tokio_timer_alone() {
    let _guard = FD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

#[test]
fn diag_0_no_request() {
    let _guard = FD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Sanity check: a runtime alone releases its fds on drop.
    let baseline = driver_fds();
    let rt = build_runtime();
    rt.block_on(async { tokio::time::sleep(Duration::from_millis(10)).await });
    drop(rt);
    assert_eq!(wait_for_baseline(baseline), baseline);
}
