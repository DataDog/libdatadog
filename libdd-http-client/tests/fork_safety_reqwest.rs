// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Fork-safety integration tests for the **reqwest** backend.
//!
//! POSIX `fork(2)` semantics: only the calling thread is duplicated in the
//! child. `reqwest::Client` is built against a tokio runtime and spawns
//! background tasks (notably the hickory-dns resolver task) on it. After
//! fork, those tasks no longer exist in the child — the underlying threads
//! were not duplicated. The contract is therefore:
//!
//! - **Rebuild the `HttpClient`** in the child after `after_fork_child`.
//!   That is the documented and supported flow.
//! - **Reusing** the parent's client is *unsupported*. The documented
//!   failure mode validated by `test_fork_without_rebuild` is that the
//!   request does not produce a successful response and resolves into a
//!   bounded error within the per-request timeout — it must NOT hang the
//!   child indefinitely.
//!
//! See `libdd-http-client/docs/fork-safety.md` for the full per-backend
//! contract.
//!
//! Linux only — see `fork_safety_hyper.rs` for rationale.

#![cfg(target_os = "linux")]

use httpmock::prelude::*;
use libdd_http_client::{HttpClient, HttpClientError, HttpMethod, HttpRequest};
use libdd_shared_runtime::SharedRuntime;
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::Pid;
use std::time::Duration;

fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Bound on how long the parent will wait for the child to finish.
///
/// Anything beyond this is treated as "the child hung" and surfaces as a
/// test failure. The child's own per-request timeouts must be tight enough
/// to fall below this ceiling.
const CHILD_WAIT_TIMEOUT: Duration = Duration::from_secs(15);

/// Per-request timeout used inside the child for the "without rebuild"
/// path. Tight enough that a hung resolver / pool socket falls into a
/// bounded `TimedOut` (or other error) rather than blocking the test.
const CHILD_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

#[cfg_attr(miri, ignore)] // miri does not support fork
#[test]
fn test_fork_with_client_rebuild() {
    ensure_crypto_provider();

    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET).path("/ping");
        then.status(200).body("pong");
    });
    let server_url = server.url("/ping");
    let base_url = server.url("/");

    let runtime = SharedRuntime::new().expect("failed to build SharedRuntime");
    // Parent builds a client to validate that rebuilding (rather than reusing)
    // is what's required. The parent client is dropped before fork via the
    // before_fork pause; the child will build a fresh one.
    let _parent_client =
        HttpClient::new(base_url.clone(), Duration::from_secs(5)).expect("client build failed");

    runtime.before_fork();

    // SAFETY: see fork_safety_hyper.rs.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed");

    if pid == 0 {
        // -- Child --
        let status = run_child_rebuild(&runtime, &base_url, &server_url);
        // SAFETY: post-fork process exit; bypass destructors and atexit hooks.
        unsafe { libc::_exit(status) };
    }

    runtime
        .after_fork_parent()
        .expect("after_fork_parent failed");
    expect_child_exit_zero(pid);
    mock.assert();
}

/// Reqwest's "without rebuild" bounded-resolution contract.
///
/// What we assert (the contract):
///   1. The child does **not hang past [`CHILD_WAIT_TIMEOUT`]**.
///   2. The child does **not panic / segfault / die from a signal**.
///   3. If the inherited client returns an `Err`, it must be one of the
///      mapped [`HttpClientError`] variants (`TimedOut`,
///      `ConnectionFailed`, `IoError`, or another mapped error) — never an
///      uncaught panic out of reqwest.
///
/// What we explicitly do **not** assert: that the inherited client
/// **fails**. To exercise the worst-case fork path the parent issues a
/// warm-up request (initialising the hickory-dns `OnceCell` and stashing
/// an idle pool socket) and the URL points at `localhost` to force the
/// resolver. Even with that setup, on reqwest 0.13 + hickory 0.25 +
/// Linux x86_64 the child often succeeds: hickory's `TokioRuntimeProvider`
/// re-spawns per-lookup tasks via `Handle::current()`, which in the child
/// is the fresh SharedRuntime, and the connection pool's idle socket is
/// just closed and reopened. Pinning the test to a specific
/// [`HttpClientError`] variant would therefore be flaky.
///
/// The child's exit code (see `run_child_no_rebuild`) records which path
/// was taken so test failures stay debuggable. The recorded outcome is
/// printed by the parent.
///
/// **Documented contract** (see `libdd-http-client/docs/fork-safety.md`):
/// callers must rebuild the client after fork. This test does **not**
/// validate that reusing the client works — it validates that doing so
/// does not hang the process.
#[cfg_attr(miri, ignore)] // miri does not support fork
#[test]
fn test_fork_without_rebuild() {
    ensure_crypto_provider();

    let server = MockServer::start();
    // Set up the mock so a rebuilt-client request would *succeed* — that way,
    // any failure we observe is structurally about the inherited client, not
    // about the mock not being there.
    let _mock = server.mock(|when, then| {
        when.method(GET).path("/ping");
        then.status(200).body("pong");
    });
    // httpmock binds to 127.0.0.1 but its `url()` returns a literal-IP URL.
    // To force the hickory-dns code path (the part of reqwest that's most
    // sensitive to fork — see `reqwest-0.13.2/src/dns/hickory.rs`), we
    // rewrite `127.0.0.1` to `localhost`. hyper-util short-circuits IP
    // literals, so a literal-IP URL would bypass the resolver entirely.
    let server_url = server
        .url("/ping")
        .replacen("127.0.0.1", "localhost", 1);
    let base_url = server.url("/").replacen("127.0.0.1", "localhost", 1);

    let runtime = SharedRuntime::new().expect("failed to build SharedRuntime");
    let client = HttpClient::builder()
        .base_url(base_url)
        .timeout(CHILD_REQUEST_TIMEOUT)
        .build()
        .expect("client build failed");

    // Warm the parent's reqwest internals before fork: this initialises the
    // hickory DNS resolver `OnceCell` on the SharedRuntime's tokio runtime
    // *and* stashes an idle HTTP/1 connection in the pool. Both of those
    // resources die in the child even though their file descriptors
    // survive — exactly the scenario the "without rebuild" contract is
    // about.
    let warmup = HttpRequest::new(HttpMethod::Get, server_url.clone());
    let warmup_resp = client
        .send_blocking(warmup, &runtime)
        .expect("parent warm-up request must succeed");
    assert_eq!(warmup_resp.status_code(), 200);

    runtime.before_fork();

    // SAFETY: see fork_safety_hyper.rs.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed");

    if pid == 0 {
        // -- Child --
        let status = run_child_no_rebuild(&runtime, &client, &server_url);
        // SAFETY: post-fork process exit.
        unsafe { libc::_exit(status) };
    }

    runtime
        .after_fork_parent()
        .expect("after_fork_parent failed");

    let exit_status = wait_with_timeout(pid, CHILD_WAIT_TIMEOUT);
    match exit_status {
        ChildOutcome::ExitedZero => {
            // The inherited client happened to work. Acceptable — DNS may
            // have been cached, or the platform's reqwest internals
            // tolerated the fork. We don't fail on this.
            eprintln!(
                "test_fork_without_rebuild: child exited 0 (inherited client happened to work — bounded outcome)"
            );
        }
        ChildOutcome::ExitedBoundedError(code) => {
            // Child reported a deterministic, bounded error (Timed out,
            // ConnectionFailed, IoError, …). This is the documented failure
            // shape. Record the exit code in case CI logs are needed for
            // debugging.
            let label = match code {
                CHILD_BOUNDED_TIMEOUT => "TimedOut",
                CHILD_BOUNDED_CONNECTION_FAILED => "ConnectionFailed",
                CHILD_BOUNDED_IO => "IoError",
                CHILD_BOUNDED_OTHER => "Other(non-success)",
                _ => "<unknown>",
            };
            eprintln!(
                "test_fork_without_rebuild: child reported bounded error {label} (exit code {code}) — this is the documented failure shape"
            );
        }
        ChildOutcome::ExitedUnexpected(code) => {
            panic!("child exited with unexpected status {code}");
        }
        ChildOutcome::Signaled(sig) => {
            panic!("child terminated by signal: {sig:?} (panic, segfault, or external kill — not a documented failure mode)");
        }
        ChildOutcome::Hung => {
            // Forcibly kill the child so the test process doesn't leak it.
            // SAFETY: we still own the child; SIGKILL is unconditional.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
                let mut s: i32 = 0;
                libc::waitpid(pid, &mut s, 0);
            }
            panic!(
                "child hung past {:?} — fork-without-rebuild contract requires bounded resolution",
                CHILD_WAIT_TIMEOUT
            );
        }
    }
}

/// Status codes for the "with rebuild" child:
/// 0 = success
/// 11 = after_fork_child failed
/// 12 = client rebuild failed
/// 13 = send_blocking returned Err
/// 14 = unexpected response (status or body)
fn run_child_rebuild(runtime: &SharedRuntime, base_url: &str, url: &str) -> i32 {
    if runtime.after_fork_child().is_err() {
        return 11;
    }

    // Rebuild the client *after* `after_fork_child`. This is the documented
    // post-fork contract for the reqwest backend. Any reqwest-internal
    // tokio tasks (e.g. hickory DNS) are now anchored to the freshly
    // reinitialised runtime owned by the SharedRuntime infra in this child.
    let client = match HttpClient::new(base_url.to_owned(), Duration::from_secs(5)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("child: client rebuild failed: {e}");
            return 12;
        }
    };

    let req = HttpRequest::new(HttpMethod::Get, url.to_owned());
    match client.send_blocking(req, runtime) {
        Ok(resp) if resp.status_code() == 200 && resp.body().as_ref() == b"pong" => 0,
        Ok(resp) => {
            eprintln!(
                "child: unexpected response status={} body={:?}",
                resp.status_code(),
                resp.body().as_ref()
            );
            14
        }
        Err(e) => {
            eprintln!("child: send_blocking after rebuild failed: {e}");
            13
        }
    }
}

/// Status codes for the "without rebuild" child. The parent only needs to
/// know "0 = unexpected success" vs "21..=24 = bounded error". Any other
/// non-zero exit lands in `ExitedUnexpected` and fails the test.
const CHILD_OK: i32 = 0;
const CHILD_BOUNDED_TIMEOUT: i32 = 21;
const CHILD_BOUNDED_CONNECTION_FAILED: i32 = 22;
const CHILD_BOUNDED_IO: i32 = 23;
const CHILD_BOUNDED_OTHER: i32 = 24;

fn run_child_no_rebuild(runtime: &SharedRuntime, client: &HttpClient, url: &str) -> i32 {
    // Reinitialise the SharedRuntime so `block_on` has a real runtime to
    // drive `send_blocking` on. This is *not* the same as rebuilding the
    // client — the client still carries handles to the parent's (now dead)
    // reqwest internals.
    if runtime.after_fork_child().is_err() {
        // Child can't make progress; treat as a bounded failure mode.
        return CHILD_BOUNDED_OTHER;
    }

    let req = HttpRequest::new(HttpMethod::Get, url.to_owned());
    let outcome = client.send_blocking(req, runtime);
    eprintln!("child(no_rebuild): outcome = {outcome:?}");
    match outcome {
        Ok(_) => CHILD_OK,
        Err(HttpClientError::TimedOut) => CHILD_BOUNDED_TIMEOUT,
        Err(HttpClientError::ConnectionFailed(_)) => CHILD_BOUNDED_CONNECTION_FAILED,
        Err(HttpClientError::IoError(_)) => CHILD_BOUNDED_IO,
        Err(_) => CHILD_BOUNDED_OTHER,
    }
}

#[derive(Debug)]
enum ChildOutcome {
    ExitedZero,
    ExitedBoundedError(i32),
    ExitedUnexpected(i32),
    Signaled(nix::sys::signal::Signal),
    Hung,
}

/// Poll `waitpid` with a wall-clock ceiling. Returns `Hung` if the child is
/// still running after `timeout`. The child is *not* killed by this function
/// — the caller must clean it up.
fn wait_with_timeout(pid: i32, timeout: Duration) -> ChildOutcome {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match waitpid(Pid::from_raw(pid), Some(nix::sys::wait::WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => {
                if std::time::Instant::now() >= deadline {
                    return ChildOutcome::Hung;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(WaitStatus::Exited(_, 0)) => return ChildOutcome::ExitedZero,
            Ok(WaitStatus::Exited(_, code))
                if (CHILD_BOUNDED_TIMEOUT..=CHILD_BOUNDED_OTHER).contains(&code) =>
            {
                return ChildOutcome::ExitedBoundedError(code);
            }
            Ok(WaitStatus::Exited(_, code)) => return ChildOutcome::ExitedUnexpected(code),
            Ok(WaitStatus::Signaled(_, sig, _)) => return ChildOutcome::Signaled(sig),
            Ok(other) => panic!("unexpected wait status: {other:?}"),
            Err(e) => panic!("waitpid failed: {e}"),
        }
    }
}

fn expect_child_exit_zero(pid: i32) {
    match waitpid(Pid::from_raw(pid), None).expect("waitpid failed") {
        WaitStatus::Exited(_, 0) => {}
        WaitStatus::Exited(_, status) => {
            panic!("child exited with status {status} (see run_child_rebuild for status code mapping)");
        }
        WaitStatus::Signaled(_, sig, _) => {
            panic!("child terminated by signal: {sig:?}");
        }
        other => panic!("unexpected wait status: {other:?}"),
    }
}
