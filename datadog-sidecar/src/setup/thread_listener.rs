// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::io;
#[cfg(unix)]
use std::os::unix::io::RawFd;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use tokio::io::unix::AsyncFd;
use tokio::sync::oneshot;
use tracing::{error, info, warn};

use crate::auth::{ConnectionAuthorizer, Decision};
use crate::config::Config;
use crate::entry::MainLoopConfig;
use crate::service::blocking::SidecarTransport;
#[cfg(target_os = "linux")]
use crate::setup::AbstractUnixSocketLiaison;
use crate::setup::Liaison;
#[cfg(not(target_os = "linux"))]
use crate::setup::SharedDirLiaison;
use libdd_ipc::{SeqpacketConn, SeqpacketListener};

static MASTER_LISTENER: OnceLock<Mutex<Option<MasterListener>>> = OnceLock::new();

/// Ensures first-connection SHM initialization runs exactly once across all threads.
static FIRST_CONNECTION_INIT: OnceLock<()> = OnceLock::new();

/// The uid and gid this listener settled on: for spotting a second uid later, and so that
/// threads started afterwards can drop themselves. See [`drop_listener_thread_privileges`].
static SERVED_IDS: OnceLock<(u32, u32)> = OnceLock::new();

/// Drop *this thread's* credentials to the uid it serves.
///
/// In thread mode the "sidecar" is a thread inside the PHP master, which is commonly root while
/// its workers run as a service account. Everything the thread creates is then created as root,
/// and anything it is pointed at it touches with root's authority (e.g. logfiles).
/// Running as the uid it actually serves removes that, and has the side benefit that the shared
/// memory it creates is owned by the right user outright instead of needing an `fchown`.
///
/// # Only this thread
///
/// This deliberately uses the raw syscall rather than `libc::setresuid`. glibc's wrapper
/// broadcasts the change to every thread in the process - which here would strip privileges from
/// the master process, which must not happen. The underlying Linux syscall is per-thread instead.
///
/// Tokio's blocking-pool threads are created from this one and so inherit the dropped
/// credentials. The watchdog thread is started earlier and keeps what it had; it only samples
/// memory usage and aborts, and creates nothing.
fn drop_listener_thread_privileges(uid: u32, gid: u32) {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: all three take no pointers and cannot fail in a way that matters here.
        let (euid, tid, pid) = unsafe { (libc::geteuid(), libc::gettid(), libc::getpid()) };

        // Nothing to drop, or nothing to drop *to*.
        if euid != 0 || uid == 0 {
            return;
        }
        if tid == pid {
            error!(
                "Refusing to drop privileges: this is the process's main thread (tid {tid}), not a sidecar thread. The host process must keep its own credentials."
            );
            return;
        }

        // Groups before the uid: dropping the uid first would remove the privilege needed for this.
        // SAFETY: setresgid/setresuid take three scalars; the raw syscall is per-thread by design.
        let gid_rc = unsafe { libc::syscall(libc::SYS_setresgid, gid, gid, gid) };
        let uid_rc = unsafe { libc::syscall(libc::SYS_setresuid, uid, uid, uid) };
        if gid_rc != 0 || uid_rc != 0 {
            error!(
                "Failed dropping sidecar thread {tid} to uid {uid}/gid {gid} (setresgid={gid_rc}, setresuid={uid_rc}); continuing as uid {euid}"
            );
            return;
        }
        info!("Sidecar listener thread {tid} dropped from root to uid {uid}/gid {gid}");
    }
    #[cfg(target_os = "macos")]
    {
        // Public in <unistd.h> and part of libSystem, but absent from the `libc` crate's Apple
        // bindings.
        extern "C" {
            fn pthread_setugid_np(uid: libc::uid_t, gid: libc::gid_t) -> libc::c_int;
        }

        // SAFETY: neither takes a pointer, and both are infallible in the sense that matters here.
        let euid = unsafe { libc::geteuid() };

        // Nothing to drop, or nothing to drop *to*. This also covers a repeat call on a thread that
        // already dropped: its euid is no longer 0, and a second switch would be refused with
        // EPERM ("the current thread cannot already be assuming another identity").
        if euid != 0 || uid == 0 {
            return;
        }
        if unsafe { libc::pthread_main_np() } != 0 {
            error!(
                "Refusing to drop privileges: this is the process's main thread, not a sidecar \
                 thread. The host process must keep its own credentials."
            );
            return;
        }

        let mut tid: u64 = 0;
        // SAFETY: writes a single u64 through a valid pointer.
        unsafe { libc::pthread_threadid_np(libc::pthread_self(), &mut tid) };

        // SAFETY: two scalars, no pointers. Per-thread by design.
        if unsafe { pthread_setugid_np(uid, gid) } != 0 {
            let err = io::Error::last_os_error();
            error!(
                "Failed dropping sidecar thread {tid} to uid {uid}/gid {gid}: {err}; \
                 continuing as uid {euid}"
            );
            return;
        }
        info!("Sidecar listener thread {tid} dropped from root to uid {uid}/gid {gid}");
    }
}

/// Drop *this* thread if the served uid is already known.
///
/// Registered as tokio's `on_thread_start`, because a current-thread runtime is not the only
/// thread the sidecar ends up with: `spawn_blocking` has its own pool, created lazily. Those
/// threads inherit the credentials of whichever thread spawned them, which is the dropped
/// listener thread in the common case - but not if tokio grows the pool from somewhere else, so
/// each one checks for itself on the way in.
///
/// Threads that start *before* the first peer is authenticated keep what they had; there is no
/// way for one thread to change another's credentials, and the uid to drop to is not known yet.
/// In practice that is the watchdog thread only, which samples memory and aborts and creates
/// nothing.
pub(crate) fn drop_thread_privileges_if_known() {
    if let Some((uid, gid)) = SERVED_IDS.get() {
        drop_listener_thread_privileges(*uid, *gid);
    }
}

pub struct MasterListener {
    shutdown_tx: Option<oneshot::Sender<()>>,
    thread_handle: Option<JoinHandle<()>>,
    /// Used by `clear_inherited_state()` to avoid forks keeping the socket open
    #[cfg(unix)]
    listener_fd: RawFd,
    /// Files to remove on an orderly shutdown, as reported by the liaison. Empty where the
    /// transport leaves nothing on disk, which is every platform but macOS today.
    bound_files: Vec<std::path::PathBuf>,
    /// The pid that bound them. A forked child inherits this struct but owns none of it, and
    /// must not remove files its parent is still listening on.
    owner_pid: u32,
}

/// Remove a listener's filesystem artifacts. Idempotent: an already-removed file is not an
/// error, which matters because more than one path can reach this.
fn reap(bound_files: &[std::path::PathBuf]) {
    for path in bound_files {
        if let Err(e) = std::fs::remove_file(path) {
            if e.kind() != io::ErrorKind::NotFound {
                warn!("Could not remove {} on shutdown: {e}", path.display());
            }
        }
    }
}

impl MasterListener {
    /// Start the master listener thread.
    ///
    /// This spawns a new OS thread that calls enter_listener_loop_with_config
    /// to create a Tokio runtime and listen for worker connections.
    /// Only one listener can be active per process.
    pub fn start(config: Config) -> io::Result<()> {
        let pid = std::process::id();

        let listener_mutex = MASTER_LISTENER.get_or_init(|| Mutex::new(None));
        let mut listener_guard = listener_mutex
            .lock()
            .map_err(|e| io::Error::other(format!("Failed to acquire listener lock: {}", e)))?;

        if listener_guard.is_some() {
            return Err(io::Error::other("Master listener is already running"));
        }

        #[cfg(target_os = "linux")]
        let liaison = AbstractUnixSocketLiaison::ipc_for_pid(pid);
        #[cfg(not(target_os = "linux"))]
        let liaison = SharedDirLiaison::ipc_for_pid(pid);

        let bound_files = liaison.bound_files();

        let listener = liaison
            .attempt_listen()?
            .ok_or_else(|| io::Error::other("Failed to create IPC listener"))?;

        #[cfg(unix)]
        let listener_fd = listener.as_raw_fd();

        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let thread_handle = thread::Builder::new()
            .name(format!("ddtrace-sidecar-listener-{}", pid))
            .spawn(move || {
                if let Err(e) = run_listener(listener, config, shutdown_rx) {
                    error!("Listener thread error: {}", e);
                }
            })
            .map_err(|e| io::Error::other(format!("Failed to spawn listener thread: {}", e)))?;

        *listener_guard = Some(MasterListener {
            shutdown_tx: Some(shutdown_tx),
            thread_handle: Some(thread_handle),
            #[cfg(unix)]
            listener_fd,
            bound_files,
            owner_pid: pid,
        });

        Ok(())
    }

    /// Shutdown the master listener thread.
    ///
    /// Sends shutdown signal and joins the listener thread. This is blocking
    /// and will wait for the thread to exit cleanly.
    pub fn shutdown() -> io::Result<()> {
        let listener_mutex = MASTER_LISTENER.get_or_init(|| Mutex::new(None));
        let mut listener_guard = listener_mutex
            .lock()
            .map_err(|e| io::Error::other(format!("Failed to acquire listener lock: {}", e)))?;

        if let Some(mut master) = listener_guard.take() {
            if let Some(tx) = master.shutdown_tx.take() {
                let _ = tx.send(());
            }

            if let Some(handle) = master.thread_handle.take() {
                handle
                    .join()
                    .map_err(|_| io::Error::other("Failed to join listener thread"))?;
            }

            // Only here, never in `clear_inherited_state`: a forked child reaches that path
            // while the parent is still listening on these very files.
            reap(&master.bound_files);

            info!("Master listener thread shut down successfully");
            Ok(())
        } else {
            Err(io::Error::other("No master listener is running"))
        }
    }

    /// Whether this process has a running master listener.
    pub fn is_active() -> bool {
        let listener_mutex = MASTER_LISTENER.get_or_init(|| Mutex::new(None));
        if let Ok(listener_guard) = listener_mutex.lock() {
            listener_guard.is_some()
        } else {
            false
        }
    }

    /// Remove the listener's files without touching the thread, for process exit.
    ///
    /// [`Self::shutdown`] is the orderly path, but it only runs if the SAPI runs PHP's module
    /// shutdown, and php-fpm's master does not: `fpm_pctl_exit()` calls `exit()` directly, so
    /// MSHUTDOWN never happens there and the socket would outlive every master. This is what
    /// an `atexit` handler calls instead.
    ///
    /// Deliberately minimal compared to `shutdown`. It does not signal or join the listener
    /// thread: at exit that thread may still be running, and joining from an exit handler could
    /// block for good. Unlinking the path while the thread still serves is harmless - the bound
    /// fd stays valid, only the name goes.
    pub fn reap_bound_files_at_exit() {
        let Some(listener_mutex) = MASTER_LISTENER.get() else {
            return;
        };
        // try_lock, never lock: an exit handler must not block on a thread that may be gone,
        // or on one that is mid-shutdown and about to reap these same files itself.
        let Ok(listener_guard) = listener_mutex.try_lock() else {
            return;
        };
        if let Some(master) = listener_guard.as_ref() {
            // A forked worker inherits the registration along with the parent's memory. Only
            // the process that bound these files may remove them.
            if master.owner_pid == std::process::id() {
                reap(&master.bound_files);
            }
        }
    }

    /// Clear inherited listener state after fork.
    ///
    /// Child processes must call this to prevent attempting to use the
    /// parent's listener thread, which doesn't exist in the child.
    pub fn clear_inherited_state() -> io::Result<()> {
        let listener_mutex = MASTER_LISTENER.get_or_init(|| Mutex::new(None));
        let mut listener_guard = listener_mutex
            .lock()
            .map_err(|e| io::Error::other(format!("Failed to acquire listener lock: {}", e)))?;

        if let Some(master) = listener_guard.take() {
            info!("Clearing inherited master listener state in child process");

            #[cfg(unix)]
            {
                // SAFETY: `fork()` duplicated this fd into this process's fd table;
                // it refers to the same listening socket the parent's listener
                // thread (which does not exist here) still owns in the parent's own
                // fd table. Closing it here only drops this process's reference and
                // has no effect on the parent.
                unsafe {
                    libc::close(master.listener_fd);
                }
            }
            #[cfg(not(unix))]
            let _ = master;
        }

        Ok(())
    }
}

/// Accept connections in a loop for thread mode.
async fn accept_socket_loop_thread(
    async_listener: AsyncFd<SeqpacketListener>,
    handler: Box<dyn Fn(SeqpacketConn)>,
    mut shutdown_rx: oneshot::Receiver<()>,
    authorizer: Arc<ConnectionAuthorizer>,
) -> io::Result<()> {
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                info!("Shutdown signal received in thread listener");
                break;
            }
            ready = async_listener.readable() => {
                match ready {
                    Ok(mut guard) => {
                        match guard.try_io(|inner| inner.get_ref().try_accept()) {
                            Ok(Ok(conn)) => {
                                // On the first connection, get the worker's UID and
                                // fchown the SHM to that UID so cross-user access works when
                                // the master runs as root and workers run as a different user.
                                if let Ok(cred) = conn.peer_credentials() {
                                    if authorizer.authorize(&cred) == Decision::Allow {
                                        FIRST_CONNECTION_INIT.get_or_init(|| {
                                            libdd_ipc::platform::set_shm_owner_uid(cred.uid);
                                            // Before the limiter is created, so it is created
                                            // owned by the uid that has to map it.
                                            drop_listener_thread_privileges(cred.uid, cred.gid);
                                            let _ = SERVED_IDS.set((cred.uid, cred.gid));
                                            crate::tracer::init_shm_limiter();
                                        });
                                        // Everything created by name belongs to the first uid
                                        // seen; say so rather than let a second pool wonder why
                                        // its remote config never arrives.
                                        if let Some((served, _)) = SERVED_IDS.get() {
                                            if *served != cred.uid {
                                                warn!(
                                                    "Peer pid {} runs as uid {}, but this listener serves uid {served}. Shared memory will not be readable by uid {}.",
                                                    cred.pid, cred.uid, cred.uid
                                                );
                                            }
                                        }
                                    }
                                }
                                handler(conn);
                            }
                            Ok(Err(e)) => {
                                error!("Failed to accept worker connection: {}", e);
                                break;
                            }
                            Err(_would_block) => continue,
                        }
                    }
                    Err(e) => {
                        error!("IPC listener error: {}", e);
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Entry point for thread listener.
///
/// Uses a single-threaded Tokio runtime (current_thread) to avoid spawning extra OS
/// threads. A multi-thread runtime would leave worker threads visible to LSan/ASAN at
/// process exit, causing "Running thread was not suspended" warnings. With
/// current_thread all async work runs on this OS thread; no other threads are created.
fn run_listener(
    listener: SeqpacketListener,
    _config: Config,
    shutdown_rx: oneshot::Receiver<()>,
) -> io::Result<()> {
    info!("Listener thread running, entering IPC server loop");

    let cancel = || {};
    let authorizer = Arc::new(ConnectionAuthorizer::for_in_process_listener());
    let loop_config = MainLoopConfig {
        enable_ctrl_c_handler: false,
        external_shutdown_rx: None,
        // Defer SHM init to first connection so we can fchown using the worker's UID.
        init_shm_eagerly: false,
        authorizer: authorizer.clone(),
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .on_thread_start(drop_thread_privileges_if_known)
        .enable_all()
        .build()
        .map_err(|e| io::Error::other(format!("Failed building tokio runtime: {}", e)))?;

    // into_async_listener() requires a Tokio reactor context, so it must be
    // called inside block_on rather than before the runtime is built.
    runtime
        .block_on(async {
            let async_listener = listener.into_async_listener()?;
            crate::entry::main_loop(
                move |handler| {
                    accept_socket_loop_thread(async_listener, handler, shutdown_rx, authorizer)
                },
                Arc::new(cancel),
                loop_config,
            )
            .await
        })
        .map_err(|e| io::Error::other(format!("Thread listener failed: {}", e)))?;

    info!("Listener thread exiting");
    Ok(())
}

/// Connect to the master listener as a worker.
///
/// Establishes a connection to the master listener thread for the given PID.
pub fn connect_to_master(pid: i32) -> io::Result<Box<SidecarTransport>> {
    info!("Connecting to master listener (PID {})", pid);

    #[cfg(target_os = "linux")]
    let liaison = AbstractUnixSocketLiaison::ipc_for_pid(pid as u32);
    #[cfg(not(target_os = "linux"))]
    let liaison = SharedDirLiaison::ipc_for_pid(pid as u32);

    let conn = liaison
        .connect_to_server()
        .map_err(|e| io::Error::other(format!("Failed to connect to master listener: {}", e)))?;

    info!("Successfully connected to master listener");
    Ok(Box::new(SidecarTransport::from(conn)))
}
