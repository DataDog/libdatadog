// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Check that a client with an idle pooled connection does not keep the tokio driver fds
//! (epoll and eventfd) open after the runtime drops, while the client lives on.

#![cfg(all(target_os = "linux", feature = "http-client", not(miri)))]

/// fd counts are process-wide, so these tests run in the single-threaded test group.
mod single_threaded_tests {
    use core::time::Duration;
    use std::time::Instant;

    use httpmock::prelude::*;
    use libdd_networking::connector::Connector;
    use libdd_networking::http_common::{self, Body, GenericHttpClient};

    /// Count the epoll and eventfd fds of this process.
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

    /// Wait until the fd counts are back to `baseline`, or give up after 2 seconds.
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

    fn start_server() -> MockServer {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/");
            then.status(200).body("ok");
        });
        server
    }

    fn build_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap()
    }

    /// Send one request from a task spawned on `rt`, as a worker would, and leave the connection
    /// idle in the pool.
    fn send_from_task(
        rt: &tokio::runtime::Runtime,
        client: &GenericHttpClient<Connector>,
        server: &MockServer,
    ) {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let client = client.clone();
        let url = server.url("/");
        rt.spawn(async move {
            let req = http::Request::get(url).body(Body::empty()).unwrap();
            let resp = client.request(req).await.unwrap();
            assert_eq!(resp.status(), 200);
            http_common::collect_response_bytes(http_common::into_response(resp))
                .await
                .unwrap();
            // Let the connection go back to the pool.
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = done_tx.send(());
        });
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn pooled_client_releases_driver_fds_after_runtime_drop() {
        let server = start_server();
        let client = http_common::new_client_periodic();
        let baseline = driver_fds();

        let rt = build_runtime();
        send_from_task(&rt, &client, &server);
        assert_ne!(driver_fds(), baseline, "the runtime should own driver fds");
        drop(rt);

        assert_eq!(wait_for_baseline(baseline), baseline);
    }

    #[test]
    fn pooled_client_works_and_releases_driver_fds_across_runtimes() {
        let server = start_server();
        let client = http_common::new_default_client();
        let baseline = driver_fds();

        for round in 0..2 {
            let rt = build_runtime();
            send_from_task(&rt, &client, &server);
            drop(rt);
            assert_eq!(wait_for_baseline(baseline), baseline, "round {round}");
        }
    }
}
