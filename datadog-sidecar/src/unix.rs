// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use spawn_worker::{SpawnWorker, Stdio, TrampolineData, getpid};

use crate::config::Config;
use crate::enter_listener_loop;
use libdd_ipc::{SeqpacketConn, SeqpacketListener};
use nix::fcntl::{F_GETFL, F_SETFL, OFlag, fcntl};
use nix::sys::socket::{Shutdown, shutdown};
use std::io;
use std::os::fd::RawFd;
use std::os::unix::prelude::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;
use tokio::select;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::Notify;
use tracing::{error, info, warn};

#[cfg(target_os = "linux")]
use crate::config::LogMethod;
#[cfg(target_os = "linux")]
use libdd_crashtracker::{
    CrashtrackerConfiguration, CrashtrackerReceiverConfig, Metadata, StacktraceCollection,
};
#[cfg(target_os = "linux")]
use spawn_worker::{entrypoint, get_dl_path_raw};
#[cfg(target_os = "linux")]
use std::ffi::CStr;

#[unsafe(no_mangle)]
#[allow(unused)]
pub extern "C" fn ddog_daemon_entry_point(trampoline_data: &TrampolineData) {
    #[cfg(feature = "tracing")]
    crate::log::enable_logging().ok();

    if let Err(err) = nix::unistd::setsid() {
        error!("Error calling setsid(): {err}")
    }

    #[cfg(target_os = "linux")]
    crate::fd_limits::raise_open_files_limit();

    #[cfg(target_os = "linux")]
    let _ = prctl::set_name("dd-ipc-helper");

    #[cfg(target_os = "linux")]
    if let Err(e) = init_crashtracker(if trampoline_data.argc > 0 {
        Some(trampoline_data.dependency_paths)
    } else {
        None
    }) {
        warn!("Failed to initialize crashtracker: {e}");
    }

    let buf_size = Config::get().pipe_buffer_size;
    if buf_size > 0 {
        libdd_ipc::platform::set_socket_buffer_size(buf_size);
    }

    let now = Instant::now();

    if let Some(fd) = spawn_worker::recv_passed_fd() {
        // Passed without close-on-exec: a child the sidecar execs would keep the name bound.
        let _ = nix::fcntl::fcntl(
            fd.as_raw_fd(),
            nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC),
        );
        let seqpacket_listener = SeqpacketListener::from_owned_fd(fd);
        info!("Starting sidecar, pid: {}", getpid());
        let acquire_listener = move || {
            // Convert to async listener (also sets non-blocking mode).
            let async_listener = seqpacket_listener.into_async_listener()?;

            // Tells the accept loop to stop: it then shuts its own listener down, which hands the
            // name over at once while the sidecar drains.
            let shutdown = Arc::new(Notify::new());
            // Watched for the whole process lifetime: tokio never uninstalls its handler, so a
            // SIGTERM arriving once the accept loop is over would be swallowed.
            let mut termsig = signal(SignalKind::terminate())?;
            tokio::spawn({
                let shutdown = shutdown.clone();
                async move {
                    if termsig.recv().await.is_some() {
                        // Armed first: logging blocks if its destination stalls.
                        crate::entry::exit_after_deadline("SIGTERM");
                        info!("Received SIGTERM, shutting down");
                        shutdown.notify_one();
                    }
                }
            });
            // Only notify: the loop stops listening on its own fd, which a callback running
            // after the loop ended would find closed, or reused by another file.
            let cancel = {
                let shutdown = shutdown.clone();
                move || {
                    // notify_one, not notify_waiters: it leaves a permit behind if the loop is
                    // between iterations, so the cancellation cannot be missed.
                    shutdown.notify_one();
                }
            };

            Ok((
                move |handler| accept_socket_loop(async_listener, handler, shutdown),
                cancel,
            ))
        };
        if let Err(err) = enter_listener_loop(acquire_listener) {
            error!("Error: {err}")
        }
    }

    info!(
        "shutting down sidecar, pid: {}, total runtime: {:.3}s",
        getpid(),
        now.elapsed().as_secs_f64()
    )
}

pub(crate) fn stop_listening(listener_fd: RawFd) {
    // We need to drop O_NONBLOCK, as accept() on a shutdown socket will just give
    // EAGAIN instead of EINVAL
    #[allow(clippy::unwrap_used)]
    let flags = OFlag::from_bits_truncate(fcntl(listener_fd, F_GETFL).ok().unwrap());
    _ = fcntl(listener_fd, F_SETFL(flags & !OFlag::O_NONBLOCK));
    _ = shutdown(listener_fd, Shutdown::Both);
}

async fn accept_socket_loop(
    async_listener: tokio::io::unix::AsyncFd<SeqpacketListener>,
    handler: Box<dyn Fn(SeqpacketConn)>,
    shutdown: Arc<Notify>,
) -> io::Result<()> {
    loop {
        select! {
            _ = shutdown.notified() => {
                stop_listening(async_listener.as_raw_fd());
                break;
            }
            ready = async_listener.readable() => {
                match ready {
                    Ok(mut guard) => {
                        match guard.try_io(|inner| inner.get_ref().try_accept()) {
                            Ok(Ok(conn)) => {
                                let buf_size = Config::get().pipe_buffer_size;
                                if buf_size > 0 {
                                    let _ = conn.set_rcvbuf_size(buf_size);
                                }
                                handler(conn);
                            }
                            Ok(Err(e)) if e.raw_os_error() == Some(libc::EMSGSIZE) => {
                                warn!("IPC accept: oversized datagram discarded (EMSGSIZE)");
                            }
                            Ok(Err(e)) => {
                                // Out of descriptors, or otherwise unable to accept: main_loop
                                // hands the name and the clients over to a fresh sidecar.
                                stop_listening(async_listener.as_raw_fd());
                                return Err(io::Error::new(
                                    e.kind(),
                                    format!("IPC accept error: {e}"),
                                ));
                            }
                            Err(_would_block) => continue,
                        }
                    }
                    Err(e) => {
                        stop_listening(async_listener.as_raw_fd());
                        return Err(io::Error::new(e.kind(), format!("IPC listener error: {e}")));
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn setup_daemon_process(
    listener: SeqpacketListener,
    spawn_cfg: &mut SpawnWorker,
) -> io::Result<()> {
    spawn_cfg
        .daemonize(true)
        .process_name("datadog-ipc-helper")
        .pass_fd(unsafe { OwnedFd::from_raw_fd(listener.into_raw_fd()) })
        .stdin(Stdio::Null);

    Ok(())
}

pub fn primary_sidecar_identifier() -> u32 {
    unsafe { libc::geteuid() }
}

/// Thread-mode master PID, or 0 for the per-user subprocess sidecar.
static THREAD_SIDECAR_PID: AtomicU32 = AtomicU32::new(0);

/// Use the shared memory of the thread-mode sidecar in `master_pid`, or of the subprocess
/// sidecar for `None`.
pub fn use_thread_sidecar_shm_namespace(master_pid: Option<u32>) {
    THREAD_SIDECAR_PID.store(master_pid.unwrap_or(0), Ordering::Relaxed);
}

/// Qualify SHM names by effective UID for subprocess sidecars and master PID for thread mode.
/// Multiple masters for one UID must not replace each other's live segments.
pub fn shm_namespace() -> ShmNamespace {
    match THREAD_SIDECAR_PID.load(Ordering::Relaxed) {
        0 => ShmNamespace::User(primary_sidecar_identifier()),
        pid => ShmNamespace::Thread(pid),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShmNamespace {
    User(u32),
    Thread(u32),
}

impl std::fmt::Display for ShmNamespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShmNamespace::User(uid) => write!(f, "{uid}"),
            ShmNamespace::Thread(pid) => write!(f, "t{pid}"),
        }
    }
}

/// Allow initializing crashtracker independently for thread-mode sidecar.
#[cfg(target_os = "linux")]
pub fn build_crashtracker_receiver_config(
    dependency_paths: Option<*const *const libc::c_char>,
    output: Option<String>,
) -> anyhow::Result<CrashtrackerReceiverConfig> {
    let entrypoint = entrypoint!(ddog_crashtracker_entry_point);
    let entrypoint_path = match unsafe { get_dl_path_raw(entrypoint.ptr as *const libc::c_void) } {
        (Some(path), _) => path,
        _ => anyhow::bail!("Failed to find crashtracker entrypoint"),
    };
    let entrypoint_path_str = entrypoint_path.into_string()?;

    let mut receiver_args = vec!["crashtracker_receiver".to_string()];
    let mut receiver_env = vec![];
    let entrypoint_name = entrypoint.symbol_name.into_string()?;

    if let Some(dependency_paths) = dependency_paths {
        receiver_args.push("".to_string());
        receiver_args.push(entrypoint_path_str.clone());
        unsafe {
            let mut descriptors = dependency_paths;
            if !descriptors.is_null() {
                loop {
                    if (*descriptors).is_null() {
                        break;
                    }
                    receiver_args.push(CStr::from_ptr(*descriptors).to_string_lossy().into_owned());
                    descriptors = descriptors.add(1);
                }
            }
        }
        receiver_args.push(entrypoint_name);
    } else {
        // direct mode: ld.so uses argv[1] as the library to exec
        receiver_args.push(entrypoint_path_str.clone());
        receiver_env.push(("_DD_SIDECAR_DIRECT_EXEC".to_string(), entrypoint_name));
        if let Ok(env) = std::env::var("_DD_SIDECAR_PATH_DEPS")
            && !env.is_empty()
        {
            receiver_env.push(("_DD_SIDECAR_PATH_DEPS".to_string(), env));
        }
    }

    CrashtrackerReceiverConfig::new(
        receiver_args,
        receiver_env,
        format!("/proc/{}/exe", unsafe { libc::getpid() }),
        output,
        None,
    )
}

#[cfg(target_os = "linux")]
fn init_crashtracker(dependency_paths: Option<*const *const libc::c_char>) -> anyhow::Result<()> {
    let output = match &Config::get().log_method {
        LogMethod::Stdout => Some(format!("/proc/{}/fd/1", unsafe { libc::getpid() })),
        LogMethod::Stderr => Some(format!("/proc/{}/fd/2", unsafe { libc::getpid() })),
        LogMethod::File(file) => file.to_str().map(|s| s.to_string()),
        LogMethod::Disabled => None,
    };
    let receiver_config = build_crashtracker_receiver_config(dependency_paths, output)?;

    let mut config_builder = CrashtrackerConfiguration::builder()
        .create_alt_stack(true)
        .use_alt_stack(true)
        .resolve_frames(StacktraceCollection::EnabledWithSymbolsInReceiver)
        .demangle_names(true);
    if let Some(ep) = Config::get().crashtracker_endpoint.as_ref() {
        config_builder = config_builder.endpoint_url(&ep.url.to_string());
        if let Some(api_key) = ep.api_key.as_deref() {
            config_builder = config_builder.endpoint_api_key(api_key);
        }
        config_builder = config_builder
            .endpoint_timeout_ms(ep.timeout_ms)
            .endpoint_use_system_resolver(ep.use_system_resolver);
        if let Some(test_token) = ep.test_token.as_deref() {
            config_builder = config_builder.endpoint_test_token(test_token);
        }
    }
    let tags = vec![
        "is_crash:true".to_string(),
        "severity:crash".to_string(),
        format!("library_version:{}", crate::sidecar_version!()),
        "library:sidecar".to_string(),
        "language:php".to_string(),
    ];

    libdd_crashtracker::init(
        config_builder.build()?,
        receiver_config,
        Metadata::new(
            "libdatadog".to_string(),
            crate::sidecar_version!().to_string(),
            "SIDECAR".to_string(),
            tags,
        ),
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_crashtracker_entry_point(_trampoline_data: &TrampolineData) {
    unsafe {
        if let Err(e) = libdd_crashtracker::receiver_entry_point_stdin() {
            eprintln!("{e}");
            libc::exit(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sidecar in a process of its own, and a client connected to it.
    ///
    /// The process re-runs the calling test, which turns into the sidecar in [`Self::serve`].
    /// The spawn trampoline cannot load the entry point from a test executable (glibc refuses to
    /// `dlopen` a PIE), and a mere fork would inherit the descriptors of concurrent tests.
    #[cfg(target_os = "linux")]
    struct TestSidecar {
        process: std::process::Child,
        transport: Option<crate::service::blocking::SidecarTransport>,
        log: std::path::PathBuf,
        name: String,
    }

    #[cfg(target_os = "linux")]
    impl TestSidecar {
        /// The sidecar of `test` (its name in this module), started with `nofile` as its soft
        /// and hard descriptor limits if given, or `None` in the sidecar process itself, which
        /// has then served as the sidecar and never returns.
        fn start(test: &str, nofile: Option<(libc::rlim_t, libc::rlim_t)>) -> Option<Self> {
            Self::start_with(test, nofile, None)
        }

        /// [`Self::start`], logging to `stderr` rather than to a file if given.
        fn start_with(
            test: &str,
            nofile: Option<(libc::rlim_t, libc::rlim_t)>,
            stderr: Option<std::process::Stdio>,
        ) -> Option<Self> {
            use std::os::unix::process::CommandExt;
            use std::time::Duration;

            let test = format!("unix::tests::{test}");
            if crate::test_util::is_rerun(&test) {
                Self::serve();
            }
            static STARTED: AtomicU32 = AtomicU32::new(0);
            let name = format!(
                "libdatadog/test-sidecar.{}.{}.sock",
                std::process::id(),
                STARTED.fetch_add(1, Ordering::Relaxed)
            );
            let log = std::env::temp_dir().join(format!("{}.log", name.replace('/', "-")));
            let mut command = crate::test_util::rerun(&test);
            command
                .env(TEST_SIDECAR_LISTENER, &name)
                .env(
                    "_DD_DEBUG_SIDECAR_LOG_METHOD",
                    format!("file://{}", log.display()),
                )
                .env("_DD_DEBUG_SIDECAR_LOG_LEVEL", "info");
            if let Some((soft, hard)) = nofile {
                let limit = libc::rlimit {
                    rlim_cur: soft,
                    rlim_max: hard,
                };
                // SAFETY: setrlimit is async-signal-safe, and only reads `limit`.
                unsafe {
                    command.pre_exec(move || {
                        if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) == 0 {
                            Ok(())
                        } else {
                            Err(io::Error::last_os_error())
                        }
                    });
                }
            }
            if let Some(stderr) = stderr {
                command
                    .env("_DD_DEBUG_SIDECAR_LOG_METHOD", "stderr")
                    .stderr(stderr);
            }
            let mut sidecar = TestSidecar {
                process: command.spawn().unwrap(),
                transport: None,
                log,
                name: name.clone(),
            };
            let started = Instant::now();
            let conn = loop {
                match SeqpacketConn::connect_abstract(name.as_bytes()) {
                    Ok(conn) => break conn,
                    Err(e) => {
                        assert!(
                            sidecar.process.try_wait().unwrap().is_none()
                                && started.elapsed() < Duration::from_secs(10),
                            "the sidecar did not start listening: {e}"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            };
            let mut transport = crate::service::blocking::SidecarTransport::from(conn);
            transport
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            crate::service::blocking::ping(&mut transport).expect("the sidecar answers");
            sidecar.transport = Some(transport);
            Some(sidecar)
        }

        /// Runs the entry point of a spawned sidecar, on the listener it would be passed.
        fn serve() -> ! {
            let name = std::env::var(TEST_SIDECAR_LISTENER).unwrap();
            let listener = SeqpacketListener::bind_abstract(name.as_bytes()).unwrap();
            // Passed as spawn_worker does, by a dup2 that leaves it inherited on exec; the SHM
            // names are kept to this process.
            let source = listener.into_raw_fd();
            // SAFETY: the target number is free in this fresh process, and the source is ours:
            // closed, so that only the passed descriptor holds the name.
            unsafe {
                assert_eq!(
                    libc::dup2(source, TEST_SIDECAR_PASSED_FD),
                    TEST_SIDECAR_PASSED_FD
                );
                libc::close(source);
            }
            unsafe {
                std::env::set_var(
                    "__DD_INTERNAL_PASSED_FD",
                    TEST_SIDECAR_PASSED_FD.to_string(),
                )
            };
            use_thread_sidecar_shm_namespace(Some(std::process::id()));
            ddog_daemon_entry_point(&TrampolineData {
                argc: 0,
                argv: std::ptr::null(),
                dependency_paths: std::ptr::null(),
            });
            // Bypass the test harness: the exit status is the entry point's.
            unsafe { libc::_exit(0) }
        }

        fn pid(&self) -> u32 {
            self.process.id()
        }

        fn log(&self) -> String {
            std::fs::read_to_string(&self.log).unwrap_or_default()
        }

        fn wait(&mut self, timeout: std::time::Duration) -> Option<std::process::ExitStatus> {
            let started = Instant::now();
            loop {
                if let Some(status) = self.process.try_wait().unwrap() {
                    return Some(status);
                }
                if started.elapsed() >= timeout {
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }

        /// No descriptor can be opened in the sidecar from now on; the open ones still work.
        fn exhaust_descriptors(&self) {
            let pid = self.pid() as libc::pid_t;
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: prlimit only reads and writes `limit`.
            unsafe {
                assert_eq!(
                    libc::prlimit(pid, libc::RLIMIT_NOFILE, std::ptr::null(), &mut limit),
                    0
                );
                limit.rlim_cur = 0;
                assert_eq!(
                    libc::prlimit(pid, libc::RLIMIT_NOFILE, &limit, std::ptr::null_mut()),
                    0
                );
            }
        }

        /// Whether a fresh sidecar could take the name over.
        fn name_is_free(&self) -> bool {
            SeqpacketListener::bind_abstract(self.name.as_bytes()).is_ok()
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for TestSidecar {
        fn drop(&mut self) {
            drop(self.transport.take());
            let _ = self.process.kill();
            let _ = self.process.wait();
            let _ = std::fs::remove_file(&self.log);
        }
    }

    #[cfg(target_os = "linux")]
    const TEST_SIDECAR_LISTENER: &str = "_DD_TEST_SIDECAR_LISTENER";
    #[cfg(target_os = "linux")]
    const TEST_SIDECAR_PASSED_FD: RawFd = 100;

    /// A queued payload must not hold the descriptor of its shared memory: payloads queue up
    /// while the agent is slow, and the sidecar ran out of descriptors.
    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn queued_shm_traces_hold_no_descriptors() {
        use crate::service::{InstanceId, SerializedTracerHeaderTags, SessionConfig, blocking};
        use libdd_common::Endpoint;
        use libdd_ipc::platform::{FileBackedHandle, ShmHandle};
        use libdd_trace_utils::msgpack_encoder::v04::to_vec_from_v04;
        use libdd_trace_utils::test_utils::create_test_no_alloc_span;
        use libdd_trace_utils::trace_utils::TracerHeaderTags;
        use std::time::Duration;

        let Some(mut sidecar) = TestSidecar::start("queued_shm_traces_hold_no_descriptors", None)
        else {
            return;
        };
        const PAYLOADS: usize = 64;
        // Accepts connections and never answers them, so the payloads stay queued.
        let agent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", agent.local_addr().unwrap());
        std::thread::spawn(move || agent.incoming().collect::<Vec<_>>());

        let open_fds = |pid| {
            std::fs::read_dir(format!("/proc/{pid}/fd"))
                .unwrap()
                .count()
        };
        let pid = sidecar.pid();
        let transport = sidecar.transport.as_mut().unwrap();
        blocking::set_connection_config(
            transport,
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
        let payload = to_vec_from_v04(&[vec![create_test_no_alloc_span(1, 2, 0, 3, true)]]);
        let before = open_fds(pid);
        for _ in 0..PAYLOADS {
            let mut mapped = ShmHandle::new(payload.len()).unwrap().map().unwrap();
            mapped.as_slice_mut()[..payload.len()].copy_from_slice(&payload);
            let headers = SerializedTracerHeaderTags::try_from(TracerHeaderTags {
                lang: "php",
                ..Default::default()
            })
            .unwrap();
            blocking::send_trace_v04_shm(transport, mapped.into(), payload.len(), headers).unwrap();
        }
        blocking::ping(transport).unwrap();
        // Each payload is mapped by a task of its own.
        std::thread::sleep(Duration::from_secs(1));
        let after = open_fds(pid);
        assert!(
            after < before + PAYLOADS / 4,
            "{PAYLOADS} queued payloads took the sidecar from {before} to {after} descriptors"
        );
    }

    /// The sidecar serves all clients, so it must not keep the soft descriptor limit of the one
    /// that spawned it, which is often 1024. It raises it up to the hard limit, short of a cap
    /// that leaves room to notice a leak, and never lowers it.
    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn the_sidecar_lifts_its_soft_open_files_limit() {
        const TEST: &str = "the_sidecar_lifts_its_soft_open_files_limit";
        if crate::test_util::is_rerun(&format!("unix::tests::{TEST}")) {
            TestSidecar::serve();
        }
        let mut own = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit only writes `own`.
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut own) }, 0);
        // The soft and hard limits at startup, and the soft limit expected then.
        for (soft, hard, expected) in [
            (256, 1_048_576, 100_000),
            (256, 4096, 4096),
            (200_000, 1_048_576, 200_000),
        ] {
            if hard > own.rlim_max {
                // Raising a hard limit takes privileges.
                eprintln!(
                    "skipped soft={soft} hard={hard}: the hard limit is {}",
                    own.rlim_max
                );
                continue;
            }
            let sidecar = TestSidecar::start(TEST, Some((soft, hard))).unwrap();
            let limits =
                std::fs::read_to_string(format!("/proc/{}/limits", sidecar.pid())).unwrap();
            let line = limits
                .lines()
                .find(|line| line.starts_with("Max open files"))
                .unwrap();
            let fields: Vec<_> = line.split_whitespace().collect();
            assert_eq!(
                (fields[3], fields[4]),
                (expected.to_string().as_str(), hard.to_string().as_str()),
                "started with soft={soft} hard={hard}: {line}"
            );
            let log = sidecar.log();
            let outcome = if expected == soft {
                format!("Left the RLIMIT_NOFILE soft limit at {soft}")
            } else {
                format!("Raised the RLIMIT_NOFILE soft limit from {soft} to {expected}")
            };
            let startup = format!("At startup, RLIMIT_NOFILE soft={soft} hard={hard}");
            assert!(
                log.contains(&startup) && log.contains(&outcome),
                "{startup:?} or {outcome:?} was not logged: {log}"
            );
        }
    }

    /// A child the sidecar execs, like the crashtracker receiver, must inherit neither its
    /// listener, which keeps the name bound, nor its clients, which then see no end of file.
    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn the_sidecar_sockets_are_close_on_exec() {
        let Some(sidecar) = TestSidecar::start("the_sidecar_sockets_are_close_on_exec", None)
        else {
            return;
        };
        let pid = sidecar.pid();
        let inherited: Vec<_> = std::fs::read_dir(format!("/proc/{pid}/fd"))
            .unwrap()
            .flatten()
            .filter(|fd| {
                std::fs::read_link(fd.path())
                    .is_ok_and(|target| target.to_string_lossy().starts_with("socket:"))
            })
            .filter_map(|fd| {
                let fd = fd.file_name().to_string_lossy().into_owned();
                let info = std::fs::read_to_string(format!("/proc/{pid}/fdinfo/{fd}")).ok()?;
                let flags = info.lines().find_map(|line| line.strip_prefix("flags:"))?;
                let flags = u32::from_str_radix(flags.trim(), 8).ok()?;
                (flags & libc::O_CLOEXEC as u32 == 0).then_some(fd)
            })
            .collect();
        assert!(
            inherited.is_empty(),
            "sockets inherited on exec: {inherited:?}"
        );
    }

    /// A sidecar out of descriptors must not linger, held by its clients: it frees its name,
    /// closes its clients so that they reconnect to a fresh sidecar, and exits well before the
    /// shutdown deadline.
    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn a_sidecar_failing_to_accept_hands_its_clients_over() {
        let Some(mut sidecar) =
            TestSidecar::start("a_sidecar_failing_to_accept_hands_its_clients_over", None)
        else {
            return;
        };
        sidecar.exhaust_descriptors();
        // Queued, but there is no descriptor left to accept it with. Refused if accept() already
        // failed: at the limit it does even with nothing queued.
        let _queued = SeqpacketConn::connect_abstract(sidecar.name.as_bytes());
        let status = sidecar.wait(std::time::Duration::from_secs(5));
        assert_eq!(
            status.and_then(|status| status.code()),
            Some(0),
            "the sidecar still runs, or hit the deadline: {status:?}"
        );
        assert!(
            sidecar.name_is_free(),
            "a fresh sidecar cannot take the name"
        );
        let log = sidecar.log();
        assert!(
            log.contains("IPC accept error:")
                && log.contains("open_fds=")
                && log.contains("RLIMIT_NOFILE soft=0 hard="),
            "the exhaustion was not reported: {log}"
        );
    }

    /// Out of descriptors, a message passing some ends only its own connection: accept() alone
    /// decides that the sidecar hands its clients over, and a client reconnecting makes it fail.
    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn a_sidecar_dropping_passed_descriptors_hands_over_once_accept_fails() {
        use crate::service::{SerializedTracerHeaderTags, blocking};
        use libdd_ipc::platform::ShmHandle;
        use libdd_trace_utils::trace_utils::TracerHeaderTags;
        use std::time::Duration;

        let Some(mut sidecar) = TestSidecar::start(
            "a_sidecar_dropping_passed_descriptors_hands_over_once_accept_fails",
            None,
        ) else {
            return;
        };
        let mut other = blocking::SidecarTransport::from(
            SeqpacketConn::connect_abstract(sidecar.name.as_bytes()).unwrap(),
        );
        other
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        blocking::ping(&mut other).expect("the other client is served");
        // accept() fails with EMFILE once at the limit even with nothing queued: let the accept
        // loop settle first, so that the receive is what runs into the limit.
        std::thread::sleep(Duration::from_millis(200));
        sidecar.exhaust_descriptors();
        let headers = SerializedTracerHeaderTags::try_from(TracerHeaderTags {
            lang: "php",
            ..Default::default()
        })
        .unwrap();
        let shm = ShmHandle::new(4096).unwrap();
        let transport = sidecar.transport.as_mut().unwrap();
        blocking::send_trace_v04_shm(transport, shm, 4096, headers).unwrap();
        assert!(
            sidecar.wait(Duration::from_secs(2)).is_none(),
            "the sidecar shut down on a receive: {}",
            sidecar.log()
        );
        assert!(!sidecar.name_is_free(), "the sidecar released its name");
        blocking::ping(&mut other).expect("the other client is still served");
        let transport = sidecar.transport.as_mut().unwrap();
        assert!(
            blocking::ping(transport).is_err(),
            "the connection survived its dropped descriptors"
        );
        assert!(
            sidecar
                .log()
                .contains("the descriptors passed with a message were dropped"),
            "the dropped descriptors were not reported: {}",
            sidecar.log()
        );

        // Queued like a worker reconnecting on its next request: there is no descriptor left to
        // accept it with.
        let _reconnecting = SeqpacketConn::connect_abstract(sidecar.name.as_bytes()).unwrap();
        let status = sidecar.wait(Duration::from_secs(5));
        assert_eq!(
            status.and_then(|status| status.code()),
            Some(0),
            "the sidecar still runs, or hit the deadline: {status:?}"
        );
        assert!(
            sidecar.name_is_free(),
            "a fresh sidecar cannot take the name"
        );
        let log = sidecar.log();
        assert!(
            log.contains("IPC accept error:") && log.contains("open_fds="),
            "the exhaustion was not reported: {log}"
        );
    }

    /// A message with more descriptors than a receive takes is that client's problem: only its
    /// connection ends, the sidecar keeps serving the others.
    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn a_message_with_too_many_descriptors_ends_only_its_connection() {
        let Some(mut sidecar) = TestSidecar::start(
            "a_message_with_too_many_descriptors_ends_only_its_connection",
            None,
        ) else {
            return;
        };
        let files: Vec<_> = (0..64)
            .map(|_| std::fs::File::open("/dev/null").unwrap())
            .collect();
        let fds: Vec<_> = files.iter().map(|file| file.as_raw_fd()).collect();
        let other = SeqpacketConn::connect_abstract(sidecar.name.as_bytes()).unwrap();
        other.send_raw_blocking(vec![0; 16], &fds).unwrap();
        assert!(
            sidecar.wait(std::time::Duration::from_secs(2)).is_none(),
            "the sidecar shut down: {}",
            sidecar.log()
        );
        let transport = sidecar.transport.as_mut().unwrap();
        crate::service::blocking::ping(transport).expect("the other client is still served");
        assert!(!sidecar.name_is_free(), "the sidecar released its name");
    }

    /// SIGTERM must end the sidecar within its deadline even while a client stays connected,
    /// like a CLI worker outside the service being stopped: it used to wait for that client.
    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn sigterm_ends_the_sidecar_despite_a_connected_client() {
        let Some(mut sidecar) =
            TestSidecar::start("sigterm_ends_the_sidecar_despite_a_connected_client", None)
        else {
            return;
        };
        let sent = Instant::now();
        assert_eq!(
            unsafe { libc::kill(sidecar.pid() as i32, libc::SIGTERM) },
            0
        );
        let status = sidecar.wait(std::time::Duration::from_secs(30));
        let elapsed = sent.elapsed();
        // Ended by the deadline, not before: the drain itself still waits for the client.
        assert_eq!(
            status.and_then(|status| status.code()),
            Some(1),
            "the sidecar still runs {elapsed:?} after SIGTERM, or ended otherwise"
        );
        let deadline = crate::entry::SHUTDOWN_DEADLINE;
        assert!(
            elapsed >= deadline && elapsed < deadline + std::time::Duration::from_secs(5),
            "exited {elapsed:?} after SIGTERM, the deadline being {deadline:?}"
        );
    }

    /// SIGTERM must end the sidecar within its deadline even when its log stalls, like a pipe
    /// nobody reads: logging then blocks, so neither arming the deadline nor exiting may wait.
    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn sigterm_ends_the_sidecar_despite_a_stalled_log() {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut pipe = [0; 2];
        // SAFETY: pipe2 only writes the two descriptors it creates.
        assert_eq!(
            unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        // SAFETY: both descriptors were just created, and are owned here only.
        let (_unread, log) =
            unsafe { (OwnedFd::from_raw_fd(pipe[0]), OwnedFd::from_raw_fd(pipe[1])) };
        // A file description of its own: non-blocking, unlike the one the sidecar writes to.
        let mut filler = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(format!("/proc/self/fd/{}", pipe[1]))
            .unwrap();
        let Some(mut sidecar) = TestSidecar::start_with(
            "sigterm_ends_the_sidecar_despite_a_stalled_log",
            None,
            Some(log.into()),
        ) else {
            return;
        };
        // Filled up: whichever line the sidecar logs next blocks.
        for chunk in [&[0u8; 4096][..], &[0]] {
            while filler.write(chunk).is_ok() {}
        }
        let sent = Instant::now();
        assert_eq!(
            unsafe { libc::kill(sidecar.pid() as i32, libc::SIGTERM) },
            0
        );
        let status =
            sidecar.wait(crate::entry::SHUTDOWN_DEADLINE + std::time::Duration::from_secs(5));
        assert_eq!(
            status.and_then(|status| status.code()),
            Some(1),
            "the sidecar still runs {:?} after SIGTERM, or ended otherwise",
            sent.elapsed()
        );
    }

    /// Without clients, SIGTERM ends the sidecar through its normal shutdown, which flushes.
    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn sigterm_without_clients_shuts_the_sidecar_down_normally() {
        let Some(mut sidecar) = TestSidecar::start(
            "sigterm_without_clients_shuts_the_sidecar_down_normally",
            None,
        ) else {
            return;
        };
        drop(sidecar.transport.take());
        assert_eq!(
            unsafe { libc::kill(sidecar.pid() as i32, libc::SIGTERM) },
            0
        );
        let status = sidecar.wait(std::time::Duration::from_secs(5));
        assert_eq!(status.and_then(|status| status.code()), Some(0));
    }

    /// Isolate the process-wide namespace change from other tests.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn thread_mode_sidecars_get_names_of_their_own() {
        // SAFETY: the child only formats names and leaves through _exit.
        match unsafe { libc::fork() } {
            -1 => panic!("fork failed"),
            0 => {
                let own = crate::tracer::shm_limiter_path();
                use_thread_sidecar_shm_namespace(Some(4242));
                let first = crate::tracer::shm_limiter_path();
                use_thread_sidecar_shm_namespace(Some(4243));
                let second = crate::tracer::shm_limiter_path();
                use_thread_sidecar_shm_namespace(None);
                let back = crate::tracer::shm_limiter_path();
                let ok = first != second && first != own && second != own && back == own;
                unsafe { libc::_exit(if ok { 0 } else { 1 }) }
            }
            pid => {
                let mut status = 0;
                assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
                assert_eq!(libc::WEXITSTATUS(status), 0, "sidecars would share names");
            }
        }
    }
}
