// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! End-to-end checks around peer authentication and the directory the socket lives in.
//!
//! The decision logic itself is unit-tested in `datadog_sidecar::auth`; what these tests cover
//! is the wiring - that `main_loop` consults the authorizer before serving a single request.

#![cfg(unix)]

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use datadog_sidecar::auth::{AuthPolicy, ConnectionAuthorizer};
use datadog_sidecar::entry::{enter_listener_loop_with_config, MainLoopConfig};
use datadog_sidecar::service::blocking::{self, SidecarTransport};
use libdd_ipc::{SeqpacketConn, SeqpacketListener};
use tokio::sync::Notify;

/// Long enough for the sidecar to wind down once told to, short enough that a hung test fails
/// rather than stalls CI.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(20);

struct Sidecar {
    thread: Option<std::thread::JoinHandle<()>>,
    stop: Arc<Notify>,
}

impl Sidecar {
    /// Run a real `main_loop` over `listener` on its own thread, under `policy`.
    fn start(listener: SeqpacketListener, policy: AuthPolicy) -> Self {
        // Mirrors the production accept loop: cancellation is signalled explicitly rather than
        // by shutting the socket down, which is a no-op on the macOS SOCK_DGRAM listener.
        let stop = Arc::new(Notify::new());
        let loop_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            let config = MainLoopConfig {
                enable_ctrl_c_handler: false,
                external_shutdown_rx: None,
                init_shm_eagerly: true,
                authorizer: Arc::new(ConnectionAuthorizer::new(policy)),
            };
            let acquire = move || {
                let async_listener = listener.into_async_listener()?;
                let cancel = {
                    let loop_stop = loop_stop.clone();
                    move || loop_stop.notify_one()
                };
                Ok((
                    move |handler| accept_loop(async_listener, handler, loop_stop),
                    cancel,
                ))
            };
            let _ = enter_listener_loop_with_config(acquire, config);
        });
        Self {
            thread: Some(thread),
            stop,
        }
    }

    /// `true` if the sidecar shut itself down within `SHUTDOWN_GRACE`.
    fn wait_for_exit(&self) -> bool {
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        let Some(thread) = self.thread.as_ref() else {
            return true;
        };
        while Instant::now() < deadline {
            if thread.is_finished() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        self.stop.notify_one();
        // Detach instead of joining unconditionally. `main_loop` does not return once it has
        // served a connection: its serve loop never observes the client's disconnect, which
        // reproduces with authorization switched off entirely (`AuthPolicy::OsEnforced`), so it
        // is not this feature's doing. Joining unconditionally would hang the test run rather
        // than fail it; a detached thread dies with the test process.
        if self.wait_for_exit() {
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

async fn accept_loop(
    async_listener: tokio::io::unix::AsyncFd<SeqpacketListener>,
    handler: Box<dyn Fn(SeqpacketConn)>,
    stop: Arc<Notify>,
) -> io::Result<()> {
    loop {
        tokio::select! {
            _ = stop.notified() => return Ok(()),
            ready = async_listener.readable() => {
                let mut guard = match ready {
                    Ok(guard) => guard,
                    Err(_) => return Ok(()),
                };
                match guard.try_io(|inner| inner.get_ref().try_accept()) {
                    Ok(Ok(conn)) => handler(conn),
                    // The listening socket went away: stop accepting.
                    Ok(Err(_)) => return Ok(()),
                    Err(_would_block) => continue,
                }
            }
        }
    }
}

fn connect(socket_path: &std::path::Path) -> SidecarTransport {
    let conn = SeqpacketConn::connect(socket_path).expect("client connect");
    let mut transport = SidecarTransport::from(conn);
    // Without this a refused connection would block the test instead of failing it.
    transport
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");
    transport
}

/// `main_loop` must consult the authorizer before serving, and serve a peer that passes. The
/// refusal side needs a peer with a foreign uid, which a test cannot produce without privileges,
/// so that is covered by the unit tests in `datadog_sidecar::auth` instead.
/// Miri cannot run this: it supports only `AF_INET`/`AF_INET6`, and every socket here is
/// `AF_UNIX`.
#[test]
#[cfg_attr(miri, ignore)]
fn an_authorized_peer_is_served() {
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let socket_path = tmpdir.path().join("auth_allow.sock");
    let listener = SeqpacketListener::bind(&socket_path).expect("bind");

    // A subprocess sidecar serves its own uid, which is what this test connects as.
    let _sidecar = Sidecar::start(listener, AuthPolicy::OwnUid);

    let mut transport = connect(&socket_path);
    blocking::ping(&mut transport).expect("a peer holding our own uid must be served");
}

/// The private-directory checks guard the socket a client will connect to, so they belong with
/// the rest of the access-control coverage.
///
/// These live in the integration binary rather than next to the code: the repair path emits a
/// `warn!`, and `datadog_sidecar::log`'s per-level counter is process-global and asserted
/// exactly by a unit test, so an in-process test here makes that assertion race.
mod directory_guard {
    // Both tests reach `private_dir::ensure`, which compares the directory's real owner against
    // `geteuid()`. Miri answers `geteuid()` with a fixed 1000 while letting the filesystem calls
    // through for real, so neither can hold there - and the one expecting a refusal would pass
    // for the wrong reason.
    use datadog_sidecar::setup::{Liaison, SharedDirLiaison};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    /// A directory of ours left writable by others is repaired - discarded and recreated - so a
    /// recoverable mistake does not permanently disable tracing.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn listening_repairs_a_permissive_directory() {
        let tmpdir = tempfile::tempdir().expect("tempdir");
        let dir = tmpdir.path().join("loose");
        fs::create_dir(&dir).expect("create");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).expect("chmod");

        let listener = SharedDirLiaison::new(&dir)
            .attempt_listen()
            .expect("a directory of ours must be repaired, not refused")
            .expect("and it must yield a listener");
        assert_eq!(
            fs::metadata(&dir).expect("stat").permissions().mode() & 0o777,
            0o700,
            "the repaired directory must no longer be writable by others"
        );
        drop(listener);
    }

    /// When the repair cannot be carried out, listening fails - and the error has to say which
    /// directory and why, because that is all an operator gets to work from.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn listening_fails_with_a_clear_error_when_the_directory_cannot_be_fixed() {
        let tmpdir = tempfile::tempdir().expect("tempdir");
        let parent = tmpdir.path().join("locked");
        fs::create_dir(&parent).expect("create");
        let dir = parent.join("loose");
        fs::create_dir(&dir).expect("create");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).expect("chmod");
        // Stands in for a directory another uid owns, which we could not remove either.
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o500)).expect("chmod");

        // Probe the precondition rather than the uid: if creating inside a directory with no
        // write permission still works, permission checks do not apply to us - we are root, as
        // in some CI images - and the failure this test inspects cannot be staged at all.
        let probe = parent.join("probe");
        if fs::write(&probe, b"").is_ok() {
            let _ = fs::remove_file(&probe);
            eprintln!(
                "skipping listening_fails_with_a_clear_error_when_the_directory_cannot_be_fixed: \
                 permission checks do not apply to this user"
            );
            return;
        }

        // `SeqpacketListener` is not Debug, so match rather than expect_err.
        let msg = match SharedDirLiaison::new(&dir).attempt_listen() {
            Err(err) => err.to_string(),
            Ok(_) => panic!("an unusable directory must not silently yield a listener"),
        };
        for expected in [
            dir.display().to_string().as_str(),
            "writable by other users",
            "could not be removed to start over",
        ] {
            assert!(
                msg.contains(expected),
                "the error must mention {expected:?}, got: {msg}"
            );
        }

        // Restore write permission so the TempDir can clean itself up.
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).expect("chmod");
    }
}
