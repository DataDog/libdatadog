// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Fork-safety integration test for the **hyper** backend.
//!
//! POSIX `fork(2)` semantics: only the calling thread is duplicated in the
//! child; threads belonging to the tokio runtime are *not*. tokio 1.x makes
//! this an unsafe state — the runtime captured at client construction time
//! is dead in the child. The contract validated here is that calling
//! [`SharedRuntime::before_fork`] before forking and
//! [`SharedRuntime::after_fork_child`] in the child reinitialises the
//! runtime and lets us drive `HttpClient::send_blocking` through it
//! against an `httpmock` server still owned by the parent.
//!
//! See `libdd-http-client/docs/fork-safety.md` for the full per-backend
//! contract.
//!
//! Linux only:
//! - macOS forbids `fork()` after Mach ports are touched (which a tokio
//!   runtime does indirectly), so the child is unstable.
//! - Windows has no `fork()`.
//!
//! Mirrors the libc-fork pattern in `libdd-common/tests/execve_integration.rs`.

#![cfg(target_os = "linux")]

use httpmock::prelude::*;
use libdd_http_client::{HttpClient, HttpMethod, HttpRequest};
use libdd_shared_runtime::SharedRuntime;
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::Pid;
use std::time::Duration;

fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg_attr(miri, ignore)] // miri does not support fork
#[test]
fn test_hyper_backend_fork_then_send_in_child() {
    ensure_crypto_provider();

    // Start the mock server in the parent. The kernel-side listener and the
    // tokio worker threads driving it stay in the parent process. The child
    // sends requests across TCP to the parent-owned socket.
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET).path("/ping");
        then.status(200).body("pong");
    });
    let server_url = server.url("/ping");
    let base_url = server.url("/");

    // Parent: build the SharedRuntime and HttpClient before fork. The hyper
    // backend's connection pool is empty at this point — no idle sockets to
    // worry about. We still call `before_fork` on the runtime to drop the
    // tokio worker threads cleanly per the SharedRuntime contract.
    let runtime = SharedRuntime::new().expect("failed to build SharedRuntime");
    let client =
        HttpClient::new(base_url.clone(), Duration::from_secs(5)).expect("failed to build client");

    runtime.before_fork();

    // SAFETY: between `before_fork` and the fork itself, no tokio worker
    // threads are running on this SharedRuntime. The MockServer keeps its
    // own runtime; the child doesn't touch that.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed");

    if pid == 0 {
        // -- Child --
        // We don't unwrap because a panic here would unwind through code the
        // child shouldn't be touching post-fork. Use `_exit` with a status
        // code so the parent can decode the failure.
        let status = run_child(&runtime, &client, &server_url);
        // SAFETY: process is exiting; `_exit` does not run destructors or
        // run atexit hooks, which is exactly what we want post-fork.
        unsafe { libc::_exit(status) };
    }

    // -- Parent --
    // Resume worker pool in the parent (none registered, but the call is the
    // documented half of the before_fork dance).
    runtime
        .after_fork_parent()
        .expect("after_fork_parent failed");

    let child_pid = Pid::from_raw(pid);
    match waitpid(child_pid, None).expect("waitpid failed") {
        WaitStatus::Exited(_, status) => {
            assert_eq!(
                status, 0,
                "child exited with status {status} (see run_child for status code mapping)"
            );
        }
        WaitStatus::Signaled(_, sig, _) => {
            panic!("child terminated by signal: {sig:?} (likely a hang or a panic post-fork)");
        }
        other => panic!("unexpected wait status: {other:?}"),
    }

    // The child performed a successful round-trip; the mock should record one hit.
    mock.assert();
}

/// Returns the exit code for the child process.
///
/// 0 = success, non-zero = a specific failure path. Keep these stable so a
/// failing test prints something useful in CI.
fn run_child(runtime: &SharedRuntime, client: &HttpClient, url: &str) -> i32 {
    // 1. Reinitialise the SharedRuntime in the child. Without this, `block_on`
    //    falls back to a temporary single-thread runtime — which would still
    //    work, but bypasses what we're trying to test.
    if runtime.after_fork_child().is_err() {
        return 10;
    }

    // 2. Send a request through the *parent-built* `HttpClient`. The hyper
    //    backend doesn't carry a DNS task or other ambient threads, so reusing
    //    it post-fork is the documented happy path.
    let req = HttpRequest::new(HttpMethod::Get, url.to_owned());
    match client.send_blocking(req, runtime) {
        Ok(resp) if resp.status_code() == 200 && resp.body().as_ref() == b"pong" => 0,
        Ok(resp) => {
            // Response received but not what we expected.
            eprintln!(
                "child: unexpected response status={} body={:?}",
                resp.status_code(),
                resp.body().as_ref()
            );
            20
        }
        Err(e) => {
            eprintln!("child: send_blocking failed: {e}");
            30
        }
    }
}
