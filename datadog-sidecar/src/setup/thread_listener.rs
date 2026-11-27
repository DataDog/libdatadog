// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::io;
#[cfg(unix)]
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use tokio::io::unix::AsyncFd;
use tokio::sync::oneshot;
use tracing::{error, info, warn};

use crate::auth::{ConnectionAuthorizer, THREAD_WORKER_IDENTITY};
use crate::config::Config;
use crate::entry::MainLoopConfig;
use crate::service::blocking::SidecarTransport;
#[cfg(target_os = "linux")]
use crate::setup::AbstractUnixSocketLiaison;
use crate::setup::Liaison;
#[cfg(not(target_os = "linux"))]
use crate::setup::SharedDirLiaison;
use libdd_ipc::{PeerCredentials, SeqpacketConn, SeqpacketListener};

static MASTER_LISTENER: OnceLock<Mutex<Option<MasterListener>>> = OnceLock::new();

/// Run this thread as the worker it serves, including when creating shared memory.
///
/// Use raw Linux syscalls: glibc's wrappers would change every thread, stripping the PHP master's
/// privileges too. macOS provides per-thread identity switching.
///
/// New Tokio threads apply the chosen identity at startup; the watchdog checks for it each tick.
fn drop_listener_thread_privileges(uid: u32, gid: u32) {
    #[cfg(target_os = "linux")]
    {
        let euid = unsafe { libc::geteuid() };
        // Nothing to drop, or nothing to drop *to*.
        if euid != 0 || uid == 0 {
            return;
        }
        // The gettid wrapper requires glibc 2.30.
        let pid = unsafe { libc::getpid() };
        let tid = unsafe { libc::syscall(libc::SYS_gettid) } as libc::pid_t;
        if tid == pid {
            error!(
                "Refusing to drop privileges: this is the process's main thread (tid {tid}), not a sidecar thread. The host process must keep its own credentials."
            );
            return;
        }

        // Groups before the uid: both need the privilege we are about to give up.
        // SAFETY: setgroups takes a count and a pointer - null is valid for a count of zero;
        // setresgid/setresuid take three scalars. The raw syscalls are per-thread by design.
        let groups_rc =
            unsafe { libc::syscall(libc::SYS_setgroups, 0, std::ptr::null::<libc::gid_t>()) };
        let gid_rc = unsafe { libc::syscall(libc::SYS_setresgid, gid, gid, gid) };
        let uid_rc = unsafe { libc::syscall(libc::SYS_setresuid, uid, uid, uid) };
        if groups_rc != 0 || gid_rc != 0 || uid_rc != 0 {
            error!(
                "Failed dropping sidecar thread {tid} to uid {uid}/gid {gid} (setgroups={groups_rc}, setresgid={gid_rc}, setresuid={uid_rc}); continuing as uid {euid}"
            );
            return;
        }
        info!("Sidecar listener thread {tid} dropped from root to uid {uid}/gid {gid}");
    }
    #[cfg(target_os = "macos")]
    {
        // Public in <unistd.h> and part of libSystem, but absent from the `libc` crate's Apple
        // bindings.
        unsafe extern "C" {
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

/// Drop this thread's privileges if the served uid is already known.
///
/// Tokio's blocking threads inherit their creator's credentials, which may still be root.
/// Register this as `on_thread_start` so each thread checks its own identity.
///
/// The watchdog starts before a worker's identity is known and calls this again each tick.
pub(crate) fn drop_thread_privileges_if_known() {
    if let Some((uid, gid)) = THREAD_WORKER_IDENTITY.get() {
        drop_listener_thread_privileges(*uid, *gid);
    }
}

/// Called on the listener thread after authentication, before dispatching requests.
pub(crate) fn initialize_worker(peer: &PeerCredentials) {
    let &(uid, gid) = THREAD_WORKER_IDENTITY.get_or_init(|| {
        let host_uid = unsafe { libc::geteuid() };
        libdd_ipc::platform::set_shm_owner_uid(peer.uid);
        if host_uid != peer.uid {
            libdd_process_utils::unix_utils::set_restrict_worker_file_outputs(true);
        }
        (peer.uid, peer.gid)
    });
    drop_listener_thread_privileges(uid, gid);
}

pub struct MasterListener {
    shutdown_tx: Option<oneshot::Sender<()>>,
    thread_handle: Option<JoinHandle<()>>,
    /// Used by `clear_inherited_state()` to avoid forks keeping the socket open. -1 once the
    /// listener thread released it, after which the number may belong to another file.
    #[cfg(unix)]
    listener_fd: Arc<AtomicI32>,
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
        if let Err(e) = std::fs::remove_file(path)
            && e.kind() != io::ErrorKind::NotFound
        {
            warn!("Could not remove {} on shutdown: {e}", path.display());
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

        if let Some(master) = listener_guard.as_ref() {
            // A listener that failed released its descriptor: a new one may take over.
            if master.owner_pid != pid || master.listener_fd.load(Ordering::Relaxed) >= 0 {
                return Err(io::Error::other("Master listener is already running"));
            }
            if let Some(released) = listener_guard.take() {
                // Before binding: the new listener uses the same paths.
                reap(&released.bound_files);
            }
        }

        #[cfg(target_os = "linux")]
        let liaison = AbstractUnixSocketLiaison::ipc_for_pid(pid);
        #[cfg(not(target_os = "linux"))]
        let liaison = SharedDirLiaison::ipc_for_pid(pid);

        let listener = liaison
            .attempt_listen()?
            .ok_or_else(|| io::Error::other("Failed to create IPC listener"))?;
        let bound_files = liaison.into_bound_files();

        #[cfg(unix)]
        let listener_fd = Arc::new(AtomicI32::new(listener.as_raw_fd()));
        let released = listener_fd.clone();

        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        crate::use_thread_sidecar_shm_namespace(Some(pid));
        let thread_handle = thread::Builder::new()
            .name(format!("ddtrace-sidecar-listener-{}", pid))
            .spawn(move || {
                if let Err(e) = run_listener(listener, config, shutdown_rx, released) {
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

    /// Whether *this* process has a running master listener.
    ///
    /// A forked child inherits the state without the thread behind it, so the pid that started
    /// the listener is what makes this true - not the mere presence of inherited memory. That
    /// keeps the question answerable here instead of leaving every caller to pair it with a
    /// pid comparison of its own.
    pub fn is_active() -> bool {
        let listener_mutex = MASTER_LISTENER.get_or_init(|| Mutex::new(None));
        if let Ok(listener_guard) = listener_mutex.lock() {
            listener_guard.as_ref().is_some_and(|master| {
                master.owner_pid == std::process::id()
                    && master.listener_fd.load(Ordering::Relaxed) >= 0
            })
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

    /// Clear inherited sidecar state after fork.
    ///
    /// # Safety
    /// Call in the child after fork, before starting threads. Inherited sidecar tasks
    /// and references to their state must not be used afterward.
    pub unsafe fn clear_inherited_state() -> io::Result<()> {
        crate::tracer::clear_inherited_state();
        unsafe { crate::service::telemetry::clear_inherited_state() };

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
                let listener_fd = master.listener_fd.load(Ordering::Relaxed);
                if listener_fd >= 0 {
                    unsafe {
                        libc::close(listener_fd);
                    }
                }
            }
            #[cfg(not(unix))]
            let _ = master;

            // Dropping the sender could wake the parent's inherited Tokio reactor.
            std::mem::forget(master);
        }

        Ok(())
    }
}

/// Releases the listener before it closes, whichever way the loop ends, including main_loop
/// dropping it: forked workers then leave its reused number alone, and a new listener may start.
struct ReleaseListener {
    listener_fd: RawFd,
    registered: Arc<AtomicI32>,
}

impl Drop for ReleaseListener {
    fn drop(&mut self) {
        crate::unix::stop_listening(self.listener_fd);
        self.registered.store(-1, Ordering::Relaxed);
    }
}

/// Accept connections in a loop for thread mode. A failure never ends the host process: the
/// listener is released instead, so that workers reconnect.
async fn accept_socket_loop_thread(
    async_listener: AsyncFd<SeqpacketListener>,
    handler: Box<dyn Fn(SeqpacketConn)>,
    mut shutdown_rx: oneshot::Receiver<()>,
    registered: Arc<AtomicI32>,
) -> io::Result<()> {
    let _release = ReleaseListener {
        listener_fd: async_listener.as_raw_fd(),
        registered,
    };
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
                                handler(conn);
                            }
                            Ok(Err(e)) => {
                                return Err(io::Error::new(
                                    e.kind(),
                                    format!("Failed to accept worker connection: {e}"),
                                ));
                            }
                            Err(_would_block) => continue,
                        }
                    }
                    Err(e) => {
                        return Err(io::Error::new(e.kind(), format!("IPC listener error: {e}")));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Run async work on the listener thread. Separate Tokio runtime workers can remain visible to
/// LSan/ASAN at process exit. Any blocking-pool threads use the privilege-drop callback below.
fn run_listener(
    listener: SeqpacketListener,
    _config: Config,
    shutdown_rx: oneshot::Receiver<()>,
    registered: Arc<AtomicI32>,
) -> io::Result<()> {
    info!("Listener thread running, entering IPC server loop");

    let cancel = || {};
    let authorizer = ConnectionAuthorizer::for_in_process_listener();
    let loop_config = MainLoopConfig {
        enable_ctrl_c_handler: false,
        external_shutdown_rx: None,
        init_shm_eagerly: false,
        authorizer,
        drain_only_own_process: true,
        enable_cdn_fetching: false,
        allow_liveness_watch: false,
        exit_on_failure: false,
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
                    accept_socket_loop_thread(async_listener, handler, shutdown_rx, registered)
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
pub fn connect_to_master(pid: i32) -> io::Result<Box<SidecarTransport>> {
    info!("Connecting to master listener (PID {})", pid);

    #[cfg(target_os = "linux")]
    let liaison = AbstractUnixSocketLiaison::ipc_for_pid(pid as u32);
    #[cfg(not(target_os = "linux"))]
    let liaison = SharedDirLiaison::ipc_for_pid(pid as u32);

    let conn = liaison
        .connect_to_server()
        .map_err(|e| io::Error::other(format!("Failed to connect to master listener: {}", e)))?;

    crate::use_thread_sidecar_shm_namespace(Some(pid as u32));
    info!("Successfully connected to master listener");
    Ok(Box::new(SidecarTransport::from(conn)))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::test_util::{is_rerun, lowest_free_fd, rerun, set_soft_nofile};
    use std::time::{Duration, Instant};

    const TEST: &str =
        "setup::thread_listener::tests::a_failed_thread_listener_releases_clients_and_registration";
    const STALLED_FLUSH_TEST: &str =
        "setup::thread_listener::tests::a_flush_cut_short_by_a_stalled_agent_stops_the_watchdog";

    /// Reports to the test from its re-run process.
    fn say(line: &str) {
        let line = format!("{line}\n");
        // SAFETY: writes a live buffer to stdout, which needs no new descriptor.
        unsafe { libc::write(1, line.as_ptr().cast(), line.len()) };
    }

    /// Re-runs `test` as the host of a listener, which reports READY once it has no descriptor
    /// left, then queues a connection the listener cannot accept. Returns the line the host
    /// reports starting with `report`, and how the host ended.
    fn fail_the_listener_of(
        test: &str,
        report: &str,
    ) -> (Option<String>, std::process::ExitStatus) {
        use std::io::{BufRead, BufReader};

        let mut host = rerun(test)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = host.id();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(60));
            // SAFETY: only signals the host, should it hang.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        });
        let mut lines = BufReader::new(host.stdout.take().unwrap())
            .lines()
            .map_while(Result::ok);
        assert!(
            lines.by_ref().any(|line| line == "READY"),
            "the listener host did not start"
        );
        let name = AbstractUnixSocketLiaison::ipc_for_pid(pid);
        // Queued, but the listener has no descriptor left to accept it with.
        let _queued =
            SeqpacketConn::connect_abstract(name.path().as_os_str().as_encoded_bytes()).unwrap();
        let result = lines.find(|line| line.starts_with(report));
        (result, host.wait().unwrap())
    }

    /// An in-process listener out of descriptors must never end its host, a PHP-FPM master. It
    /// closes its clients, releases its registration so that a new listener can start, and
    /// forked workers must leave alone the descriptor now reusing its number.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_failed_thread_listener_releases_clients_and_registration() {
        if is_rerun(TEST) {
            host_a_failing_listener();
        }
        let (result, status) = fail_the_listener_of(TEST, "closed=");
        assert_eq!(
            result.as_deref(),
            Some("closed=true released=true fd_kept_in_forks=true restarted=true"),
            "host status {status:?}"
        );
        assert_eq!(status.code(), Some(0), "the host process did not stay up");
    }

    /// The re-run test process: a PHP-FPM master with a listener that runs out of descriptors.
    fn host_a_failing_listener() -> ! {
        let pid = std::process::id();
        let config = crate::config::FromEnv::config();
        MasterListener::start(config.clone()).unwrap();
        let listener_fd = MASTER_LISTENER
            .get()
            .and_then(|master| {
                master
                    .lock()
                    .ok()?
                    .as_ref()?
                    .listener_fd
                    .load(Ordering::Relaxed)
                    .into()
            })
            .unwrap();
        let mut transport = connect_to_master(pid as i32).unwrap();
        transport
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        crate::service::blocking::ping(&mut transport).unwrap();

        // No free slot below the limit, which poll() needs to be above zero.
        let soft = set_soft_nofile(lowest_free_fd() as libc::rlim_t);
        say("READY");
        let ready = Instant::now();
        let fd = transport.as_raw_fd();
        let mut closed = false;
        while !closed && ready.elapsed() < Duration::from_secs(15) {
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: poll and recv only access `pfd` and a live buffer.
            unsafe {
                if libc::poll(&mut pfd, 1, 200) > 0 {
                    let mut byte = 0u8;
                    let n = libc::recv(fd, (&raw mut byte).cast(), 1, libc::MSG_DONTWAIT);
                    closed = n == 0
                        || (n < 0
                            && io::Error::last_os_error().kind() != io::ErrorKind::WouldBlock);
                }
            }
        }
        set_soft_nofile(soft);
        // Past the deadline a subprocess sidecar exits by: this host must not.
        std::thread::sleep(
            (crate::entry::SHUTDOWN_DEADLINE + Duration::from_secs(1))
                .saturating_sub(ready.elapsed()),
        );

        let released = !MasterListener::is_active();
        // The number of the closed listener, now another file.
        let file = std::fs::File::open("/dev/null").unwrap();
        // SAFETY: the old number is free; dup2 makes it refer to /dev/null.
        assert_eq!(
            unsafe { libc::dup2(file.as_raw_fd(), listener_fd) },
            listener_fd
        );
        // SAFETY: the forked child only clears the inherited state, probes and leaves.
        let fd_kept_in_forks = match unsafe { libc::fork() } {
            0 => unsafe {
                let _ = MasterListener::clear_inherited_state();
                libc::_exit(if libc::fcntl(listener_fd, libc::F_GETFD) == -1 {
                    1
                } else {
                    0
                })
            },
            child => {
                let mut status = 0;
                // SAFETY: waits for our own child.
                unsafe { libc::waitpid(child, &mut status, 0) };
                libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0
            }
        };
        let restarted = MasterListener::start(config).is_ok();
        say(&format!(
            "closed={closed} released={released} fd_kept_in_forks={fd_kept_in_forks} \
             restarted={restarted}"
        ));
        // Bypass the test harness, and any listener thread left.
        unsafe { libc::_exit(0) }
    }

    /// Once the listener failed, a stalled agent cuts the flush short. The watchdog must stop all
    /// the same: its thread outlives the listener's runtime, and would abort the host.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_flush_cut_short_by_a_stalled_agent_stops_the_watchdog() {
        if is_rerun(STALLED_FLUSH_TEST) {
            host_a_listener_failing_with_a_stalled_agent();
        }
        let (result, status) = fail_the_listener_of(STALLED_FLUSH_TEST, "cut_short=");
        assert_eq!(
            result.as_deref(),
            Some("cut_short=true watchdog_stopped=true"),
            "host status {status:?}"
        );
        assert_eq!(status.code(), Some(0), "the host process did not stay up");
    }

    /// The re-run test process: a host whose listener fails while a trace is sent to an agent
    /// that never answers.
    fn host_a_listener_failing_with_a_stalled_agent() -> ! {
        use crate::service::{InstanceId, SerializedTracerHeaderTags, SessionConfig, blocking};
        use libdd_common::Endpoint;
        use libdd_trace_utils::msgpack_encoder::v04::to_vec_from_v04;
        use libdd_trace_utils::test_utils::create_test_no_alloc_span;
        use libdd_trace_utils::trace_utils::TracerHeaderTags;

        let agent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", agent.local_addr().unwrap());
        let (connected, sending) = std::sync::mpsc::channel();
        // Holds the first connection without ever answering it, nor accepting any other.
        std::thread::spawn(move || {
            let _held = agent.accept();
            let _ = connected.send(());
            loop {
                std::thread::park();
            }
        });
        let pid = std::process::id();
        MasterListener::start(crate::config::FromEnv::config()).unwrap();
        let mut transport = connect_to_master(pid as i32).unwrap();
        transport
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        blocking::set_connection_config(
            &mut transport,
            InstanceId::new("session", "runtime"),
            SessionConfig {
                endpoint: Endpoint {
                    url: url.parse().unwrap(),
                    ..Endpoint::default()
                },
                flush_interval: Duration::from_millis(100),
                ..SessionConfig::for_test()
            },
        )
        .unwrap();
        let headers = SerializedTracerHeaderTags::try_from(TracerHeaderTags {
            lang: "php",
            ..Default::default()
        })
        .unwrap();
        let trace = to_vec_from_v04(&[vec![create_test_no_alloc_span(1, 2, 0, 3, true)]]);
        blocking::send_trace_v04_bytes(&mut transport, trace, headers).unwrap();
        sending
            .recv_timeout(Duration::from_secs(10))
            .expect("the trace was not sent");

        let soft = set_soft_nofile(lowest_free_fd() as libc::rlim_t);
        say("READY");
        let ready = Instant::now();
        let listener_ended = || {
            let master = MASTER_LISTENER.get().unwrap().lock().unwrap();
            let thread = &master.as_ref().unwrap().thread_handle;
            thread.as_ref().is_some_and(JoinHandle::is_finished)
        };
        while !listener_ended() && ready.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(100));
        }
        let cut_short = listener_ended() && ready.elapsed() >= crate::entry::FAILED_FLUSH_TIMEOUT;
        set_soft_nofile(soft);
        // Its thread aborts the process unless the watchdog saw the shutdown.
        let watchdog_running = || {
            std::fs::read_dir("/proc/self/task")
                .unwrap()
                .flatten()
                .any(|task| {
                    std::fs::read_to_string(task.path().join("comm"))
                        .is_ok_and(|name| name.trim_end() == "dd-watchdog")
                })
        };
        while watchdog_running() && ready.elapsed() < Duration::from_secs(45) {
            std::thread::sleep(Duration::from_millis(100));
        }
        say(&format!(
            "cut_short={cut_short} watchdog_stopped={}",
            !watchdog_running()
        ));
        // Bypass the test harness, and the threads left.
        unsafe { libc::_exit(0) }
    }
}
