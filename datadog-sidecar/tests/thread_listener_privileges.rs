// Copyright 2026 Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(target_os = "linux")]

use datadog_sidecar::auth::{ConnectionAuthorizer, Decision};
use datadog_sidecar::config::Config;
use datadog_sidecar::entry::{MainLoopConfig, main_loop};
use datadog_sidecar::service::blocking;
use datadog_sidecar::setup::{
    AbstractUnixSocketLiaison, Liaison, MasterListener, connect_to_master,
};
use libdd_ipc::PeerCredentials;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

const WORKER_UID: u32 = 61001;
const WORKER_GID: u32 = 61002;
const WORKER_TARGET: &str = "DD_AUTH_TEST_TARGET";
const WORKER_ALLOWED: &str = "DD_AUTH_TEST_ALLOWED";

fn worker(uid: u32, gid: u32, allowed: bool) {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "thread_listener_authentication_and_restart",
            "--nocapture",
        ])
        .env(WORKER_TARGET, std::process::id().to_string())
        .env(WORKER_ALLOWED, allowed.to_string())
        .uid(uid)
        .gid(gid)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "worker {uid}/{gid}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn listener_uses_worker_credentials(stage: &str) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let dropped = std::fs::read_dir("/proc/self/task").unwrap().any(|entry| {
            let path = entry.unwrap().path();
            let name = std::fs::read_to_string(path.join("comm")).unwrap_or_default();
            if !name.starts_with("ddtrace-sidecar") {
                return false;
            }
            let status = std::fs::read_to_string(path.join("status")).unwrap();
            [("Uid:", WORKER_UID), ("Gid:", WORKER_GID)]
                .iter()
                .all(|(field, expected)| {
                    status.lines().any(|line| {
                        line.starts_with(field)
                            && line
                                .split_whitespace()
                                .skip(1)
                                .all(|id| id.parse::<u32>().unwrap() == *expected)
                    })
                })
        });
        if dropped {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{stage}: listener kept the host's credentials"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(unsafe { libc::geteuid() }, 0, "the host must remain root");
}

#[test]
fn thread_listener_authentication_and_restart() {
    if let Ok(target) = std::env::var(WORKER_TARGET) {
        let allowed: bool = std::env::var(WORKER_ALLOWED).unwrap().parse().unwrap();
        let mut transport = connect_to_master(target.parse().unwrap()).unwrap();
        transport
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        assert_eq!(blocking::ping(&mut transport).is_ok(), allowed);
        return;
    }
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("requires root to start workers with different UIDs and GIDs");
        return;
    }

    // A rejected connection must not claim the identity or initialize worker SHM.
    let listener = AbstractUnixSocketLiaison::ipc_for_pid(std::process::id())
        .attempt_listen()
        .unwrap()
        .unwrap();
    let rejecting = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(main_loop(
                move |handler| async move {
                    let listener = listener.into_async_listener()?;
                    loop {
                        let mut ready = listener.readable().await?;
                        if let Ok(conn) = ready.try_io(|inner| inner.get_ref().try_accept()) {
                            handler(conn?);
                            return Ok(());
                        }
                    }
                },
                Arc::new(|| {}),
                MainLoopConfig {
                    enable_ctrl_c_handler: false,
                    init_shm_eagerly: false,
                    ..Default::default()
                },
            ))
            .unwrap();
    });
    worker(WORKER_UID + 1, WORKER_GID, false);
    rejecting.join().unwrap();
    let auth = ConnectionAuthorizer::for_in_process_listener();
    assert_eq!(
        auth.authorize(&PeerCredentials {
            pid: std::process::id(),
            uid: WORKER_UID,
            gid: WORKER_GID,
        }),
        Decision::Allow
    );

    MasterListener::start(Config::get().clone()).unwrap();
    worker(WORKER_UID, WORKER_GID, true);
    listener_uses_worker_credentials("initial start");
    let shm_path = format!(
        "/dev/shm{}",
        datadog_sidecar::tracer::shm_limiter_path().to_string_lossy()
    );
    assert_eq!(std::fs::metadata(shm_path).unwrap().uid(), WORKER_UID);
    worker(WORKER_UID, WORKER_GID + 1, false);
    worker(WORKER_UID + 1, WORKER_GID, false);
    MasterListener::shutdown().unwrap();

    MasterListener::start(Config::get().clone()).unwrap();
    worker(WORKER_UID, WORKER_GID, true);
    listener_uses_worker_credentials("restart");
    worker(WORKER_UID + 1, WORKER_GID, false);
    worker(WORKER_UID, WORKER_GID + 1, false);
    MasterListener::shutdown().unwrap();
}
