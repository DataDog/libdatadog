// Copyright 2026 Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

use datadog_sidecar::config::Config;
use datadog_sidecar::service::blocking;
use datadog_sidecar::service::exception_hash_rate_limiter::ExceptionHashRateLimiter;
use datadog_sidecar::service::telemetry::get_telemetry_action_sender;
use datadog_sidecar::setup::{MasterListener, connect_to_master};
use datadog_sidecar::tracer::shm_limiter_path;
use libdd_ipc::rate_limiter::ShmLimiterMemory;
use std::time::{Duration, Instant};

#[test]
#[cfg_attr(miri, ignore)]
fn a_forked_worker_can_start_its_own_listener() {
    MasterListener::start(Config::get().clone()).unwrap();
    let mut parent = connect_to_master(std::process::id().try_into().unwrap()).unwrap();
    parent
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    blocking::ping(&mut parent).unwrap();
    let parent_path = shm_limiter_path();
    let parent_arena = ShmLimiterMemory::<()>::open(&parent_path).unwrap();

    for promote in [false, true] {
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert!(get_telemetry_action_sender().is_ok());
                unsafe { MasterListener::clear_inherited_state() }.unwrap();
                assert!(!MasterListener::is_active());
                assert!(get_telemetry_action_sender().is_err());
                if !promote {
                    return;
                }
                MasterListener::start(Config::get().clone()).unwrap();
                let mut child = connect_to_master(std::process::id().try_into().unwrap()).unwrap();
                child
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                blocking::ping(&mut child).unwrap();

                let child_path = shm_limiter_path();
                assert_ne!(child_path, parent_path);
                assert!(ShmLimiterMemory::<()>::open(&child_path).is_ok());
                assert!(ExceptionHashRateLimiter::open().is_ok());
                assert!(!parent_arena.is_retired());
                drop(child);
                MasterListener::shutdown().unwrap();
            }));
            // Run the SHM exit cleanup without dropping inherited stack values.
            std::process::exit(if result.is_ok() { 0 } else { 1 });
        }

        let deadline = Instant::now() + Duration::from_secs(20);
        let mut status = 0;
        loop {
            let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if waited == pid {
                break;
            }
            assert_eq!(waited, 0, "waitpid failed");
            if Instant::now() >= deadline {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                    libc::waitpid(pid, &mut status, 0);
                }
                panic!("child blocked on inherited listener state");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 0);
        assert!(ShmLimiterMemory::<()>::open(&parent_path).is_ok());
        assert!(!parent_arena.is_retired());
        blocking::ping(&mut parent).unwrap();
    }
    drop(parent);
    MasterListener::shutdown().unwrap();
}
