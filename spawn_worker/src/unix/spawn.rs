// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

use std::fs::File;
use std::os::fd::IntoRawFd;
use std::{
    env,
    ffi::{self, CString, OsString},
    fs::Permissions,
    io::{Read, Seek, Write},
    mem::ManuallyDrop,
    os::unix::prelude::{
        AsFd, AsRawFd, BorrowedFd, FromRawFd, OsStrExt, OsStringExt, PermissionsExt, RawFd,
    },
};

use io_lifetimes::OwnedFd;
use nix::{libc, sys::wait::WaitStatus, unistd::Pid};

use crate::fork::{
    clear_signal_mask, fork_skip_atfork_handlers, reset_signal_dispositions,
    KeepChildSignalsBlocked, RawFork,
};
use crate::unix::spawn::helper::ExecVec;
use crate::{LibDependency, Target};

#[derive(Clone)]
pub enum SpawnMethod {
    #[cfg(target_os = "linux")]
    Direct,
    #[cfg(target_os = "linux")]
    FdExec,
    #[cfg(not(target_os = "macos"))]
    LdPreload,
    Exec,
}

impl Target {
    /// TODO: ld_preload type trampoline is not yet supported on osx
    /// loading executables as shared libraries with dlload + dlsym however seems to work ok?
    #[cfg(target_os = "macos")]
    pub fn detect_spawn_method(&self) -> std::io::Result<SpawnMethod> {
        Ok(SpawnMethod::Exec)
    }

    /// Automatically detect which spawn method should be used
    #[cfg(not(target_os = "macos"))]
    pub fn detect_spawn_method(&self) -> std::io::Result<SpawnMethod> {
        if let Ok(env) = env::var("DD_SPAWN_WORKER_USE_EXEC") {
            if !env.is_empty() {
                return Ok(SpawnMethod::Exec);
            }
        }

        let current_exec_path = env::current_exe()?;
        let current_exec_filename = current_exec_path.file_name().unwrap_or_default();
        #[cfg(target_os = "linux")]
        let default_method = SpawnMethod::FdExec;

        #[cfg(not(target_os = "linux"))]
        let default_method = SpawnMethod::Exec;

        let target_path = match self {
            Target::Entrypoint(e) => e
                .get_fs_path()
                .ok_or_else(|| std::io::Error::other("can't find the entrypoint's target path")),
            Target::ManualTrampoline(p, _) => Ok(std::path::PathBuf::from(p)),
        }?;
        let target_filename = target_path
            .file_name()
            .ok_or_else(|| std::io::Error::other("can't extract actual target filename"))?;

        // simple heuristic that should cover most cases
        // if both executable path and target's entrypoint path end up having the same filenames
        // then it means its not a shared library - and we need to load the trampoline us ld_preload
        if current_exec_filename == target_filename {
            Ok(SpawnMethod::LdPreload)
        } else {
            Ok(default_method)
        }
    }
}

pub enum Stdio {
    Inherit,
    Fd(OwnedFd),
    Null,
}

impl From<&File> for Stdio {
    fn from(val: &File) -> Self {
        Stdio::Fd(val.try_clone().unwrap().into())
    }
}

pub struct SpawnWorker {
    stdin: Stdio,
    stderr: Stdio,
    stdout: Stdio,
    daemonize: bool,
    spawn_method: Option<SpawnMethod>,
    fd_to_pass: Option<OwnedFd>,
    target: Target,
    shared_lib_dependencies: Vec<LibDependency>,
    env: Vec<(ffi::OsString, ffi::OsString)>,
    process_name: Option<String>,
}

impl SpawnWorker {
    pub fn from_env<T: Into<Target>, E: IntoIterator<Item = (ffi::OsString, ffi::OsString)>>(
        target: T,
        env: E,
    ) -> Self {
        Self {
            stdin: Stdio::Inherit,
            stdout: Stdio::Inherit,
            stderr: Stdio::Inherit,
            daemonize: false,
            target: target.into(),
            spawn_method: None,
            fd_to_pass: None,
            env: env.into_iter().collect(),
            process_name: None,
            shared_lib_dependencies: vec![],
        }
    }

    /// # Safety
    /// since the rust library code can coexist with other code written in other languages
    /// access to environment (required to be read to be passed to subprocess) is unsafe
    ///
    /// ensure no other threads read the environment at the same time as this method is called
    pub unsafe fn new<T: Into<Target>>(target: T) -> Self {
        Self::from_env(target, env::vars_os())
    }

    pub fn shared_lib_dependencies(&mut self, deps: Vec<LibDependency>) -> &mut Self {
        self.shared_lib_dependencies = deps;
        self
    }

    pub fn process_name<S: Into<String>>(&mut self, process_name: S) -> &mut Self {
        self.process_name = Some(process_name.into());
        self
    }

    pub fn stdin<S: Into<Stdio>>(&mut self, stdio: S) -> &mut Self {
        self.stdin = stdio.into();
        self
    }

    pub fn stdout<S: Into<Stdio>>(&mut self, stdio: S) -> &mut Self {
        self.stdout = stdio.into();
        self
    }

    pub fn daemonize(&mut self, daemonize: bool) -> &mut Self {
        self.daemonize = daemonize;
        self
    }

    pub fn stderr<S: Into<Stdio>>(&mut self, stdio: S) -> &mut Self {
        self.stderr = stdio.into();
        self
    }

    pub fn spawn_method(&mut self, spawn_method: SpawnMethod) -> &mut Self {
        self.spawn_method = Some(spawn_method);
        self
    }

    pub fn pass_fd<T: Into<OwnedFd>>(&mut self, fd: T) -> &mut Self {
        self.fd_to_pass = Some(fd.into());
        self
    }

    pub fn append_env<K: Into<OsString>, V: Into<OsString>>(
        &mut self,
        key: K,
        value: V,
    ) -> &mut Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Set an env var, removing any existing entry with the same key first.
    /// Use this instead of `append_env` when the parent process may already
    /// have the variable set and the child must use the new value.
    pub fn set_env<K: Into<OsString>, V: Into<OsString>>(&mut self, key: K, value: V) -> &mut Self {
        let key = key.into();
        self.env.retain(|(k, _)| k != &key);
        self.env.push((key, value.into()));
        self
    }

    /// Wait for the first child to exit successfully, reporting setup and exec
    /// failures from either child when daemonizing.
    pub fn wait_spawn(&mut self) -> anyhow::Result<()> {
        self.spawn()?.wait_success()
    }

    /// Spawn a worker and return after the first fork without waiting for child setup or exec.
    /// Call `Child::wait()` to report child setup/exec failures and reap the first child.
    pub fn spawn(&mut self) -> anyhow::Result<Child> {
        // Resolve the spawn method, target, and initial argument vector.
        #[allow(unused_mut)]
        let mut spawn_method = match &self.spawn_method {
            Some(m) => m.clone(),
            None => self.target.detect_spawn_method()?,
        };

        let mut argv = ExecVec::empty();

        // On Linux, Direct mode uses env vars for deps instead of argv entries.
        #[cfg(target_os = "linux")]
        let use_direct = matches!(spawn_method, SpawnMethod::Direct);
        #[cfg(not(target_os = "linux"))]
        let use_direct = false;

        // set argv[0] and process name shown eg in `ps`
        let process_name = CString::new(self.process_name.as_deref().unwrap_or("spawned_worker"))?;
        argv.push(process_name);

        let (entrypoint_object, entrypoint_symbol_name) = match &self.target {
            Target::Entrypoint(entrypoint) => {
                let path = match unsafe {
                    crate::get_dl_path_raw(entrypoint.ptr as *const libc::c_void)
                } {
                    (Some(path), _) => path,
                    _ => return Err(anyhow::format_err!("can't read symbol pointer data")),
                };

                (path, entrypoint.symbol_name.clone())
            }
            Target::ManualTrampoline(path, symbol_name) => (
                CString::new(path.as_str())?,
                CString::new(symbol_name.as_str())?,
            ),
        };

        if !use_direct {
            argv.push(CString::new("")?);
        }
        argv.push(entrypoint_object);

        // Build the child environment.
        let mut envp = ExecVec::empty();
        for (k, v) in &self.env {
            // reserve space for '=' and final null
            let mut env_entry = OsString::with_capacity(k.len() + v.len() + 2);
            env_entry.push(k);
            env_entry.reserve(v.len() + 2);
            env_entry.push("=");
            env_entry.push(v);

            if let Ok(env_entry) = CString::new(env_entry.into_vec()) {
                envp.push(env_entry);
            }
        }

        let fd_to_pass = self.fd_to_pass.as_ref();
        if fd_to_pass.is_some() {
            // The passed descriptor is always installed as fd 3.
            envp.push(CString::new(format!("{}={}", crate::ENV_PASS_FD_KEY, 3))?);
        }

        let mut temp_files = TempFiles::new();
        #[cfg(target_os = "linux")]
        let mut temp_memfds = vec![];

        #[cfg(target_os = "linux")]
        let direct_ld_cstr = if use_direct {
            let mut env = b"_DD_SIDECAR_DIRECT_EXEC=".to_vec();
            env.extend_from_slice(entrypoint_symbol_name.as_bytes_with_nul());
            envp.push(CString::from_vec_with_nul(env)?);
            // Binary deps (mock_php) skipped: PHP symbols are weakened in .dynsym.
            let path_deps: Vec<String> = self
                .shared_lib_dependencies
                .iter()
                .filter_map(|dep| match dep {
                    LibDependency::Path(p) => Some(p.to_string_lossy().into_owned()),
                    LibDependency::Binary(_) => None,
                })
                .collect();
            if !path_deps.is_empty() {
                if let Ok(s) =
                    CString::new(format!("_DD_SIDECAR_PATH_DEPS={}", path_deps.join(":")))
                {
                    envp.push(s);
                }
            }

            Some(CString::new(
                crate::read_pt_interp_self()
                    .ok_or_else(|| {
                        anyhow::format_err!("Direct spawn: no PT_INTERP in current process")
                    })?
                    .to_str()
                    .ok_or_else(|| anyhow::format_err!("non-UTF8 interp path"))?,
            )?)
        } else {
            None
        };

        // Prepare shared-library dependencies and their argument entries.
        if !use_direct {
            for dep in &self.shared_lib_dependencies {
                match dep {
                    LibDependency::Path(path) => {
                        argv.push(CString::new(path.to_string_lossy().to_string())?)
                    }
                    LibDependency::Binary(bin) => {
                        let mut tempfile = || -> anyhow::Result<()> {
                            let path = temp_files.persist(bin)?;
                            argv.push(CString::new("-")?);
                            argv.push(path);
                            Ok(())
                        };
                        #[cfg(target_os = "linux")]
                        if matches!(spawn_method, SpawnMethod::FdExec) {
                            if let Ok(memfd) = linux::write_memfd("trampoline_dependencies.so", bin)
                            {
                                // This number is appended to /proc/self/fd/ below and must match
                                // where the child plan installs the memfd. The passed fd reserves
                                // fd 3; dependency memfds occupy the following fds in order.
                                let destination =
                                    ChildFdPlan::dependency_destination(fd_to_pass.is_some())
                                        + RawFd::try_from(temp_memfds.len())?;
                                argv.push(CString::new(format!("/proc/self/fd/{}", destination))?);
                                temp_memfds.push(OwnedFd::from(memfd.into_file()));
                            } else {
                                spawn_method = SpawnMethod::Exec;
                                tempfile()?;
                            }
                        } else {
                            tempfile()?;
                        }
                        #[cfg(not(target_os = "linux"))]
                        tempfile()?;
                    }
                }
            }

            argv.push(entrypoint_symbol_name);
        }

        // Prepare child descriptors and reserve space for the executable source.
        #[cfg(target_os = "linux")]
        let dependency_fds = temp_memfds.into_iter();
        #[cfg(not(target_os = "linux"))]
        let dependency_fds = std::iter::empty();
        let (fd_plan, status_read) = ChildFdPlan::new(
            &self.stdin,
            &self.stdout,
            &self.stderr,
            fd_to_pass.map(|fd| fd.as_fd()),
            dependency_fds,
        )?;

        // Prepare every executable and fallback path before forking. FdExec
        // materializes its fallback only if execveat fails.
        let prepared_spawn = match spawn_method {
            #[cfg(target_os = "linux")]
            SpawnMethod::Direct => {
                let ld_cstr = direct_ld_cstr.ok_or_else(|| {
                    std::io::Error::other("dynamic linker not prepared for Direct spawn")
                })?;
                PreparedSpawn::Direct(ld_cstr)
            }
            #[cfg(target_os = "linux")]
            SpawnMethod::FdExec => {
                let fallback_path = linux::prepare_fallback_path().ok();
                let fd = linux::write_trampoline()?;
                let fd = fd_plan.duplicate_source(fd.as_raw_fd())?;
                PreparedSpawn::FdExec { fd, fallback_path }
            }
            #[cfg(not(target_os = "macos"))]
            SpawnMethod::LdPreload => {
                // This file leaks after a successful exec because the LD_PRELOAD
                // trampoline does not unlink itself.
                let lib_path = temp_files.persist(crate::LD_PRELOAD_TRAMPOLINE_LIB)?;
                let env_prefix = "LD_PRELOAD=";

                let mut ld_env =
                    OsString::with_capacity(env_prefix.len() + lib_path.as_bytes().len() + 1);

                ld_env.push(env_prefix);
                ld_env.push(ffi::OsStr::from_bytes(lib_path.as_bytes()));
                envp.push(CString::new(ld_env.into_vec())?);

                let path = CString::new(env::current_exe()?.as_os_str().as_bytes())?;

                argv.set(1, path.clone());
                PreparedSpawn::LdPreload(path)
            }
            SpawnMethod::Exec => {
                let path = temp_files.persist(crate::TRAMPOLINE_BIN)?;

                argv.set(1, path.clone());
                PreparedSpawn::Exec(path)
            }
        };

        self.spawn_prepared(fd_plan, status_read, prepared_spawn, argv, envp, temp_files)
    }

    fn spawn_prepared(
        &self,
        mut fd_plan: ChildFdPlan<'_>,
        status_read: File,
        prepared_spawn: PreparedSpawn,
        argv: ExecVec,
        envp: ExecVec,
        mut temp_files: TempFiles,
    ) -> anyhow::Result<Child> {
        fd_plan.reserve_directory_fd()?;

        let daemonize = self.daemonize;

        // No allocation, formatting, unwinding, or owned-value destruction is
        // permitted in the child from this point through exec.

        // Keep a daemonizing child protected from inherited handlers throughout
        // its descriptor setup, session creation, and second fork.
        let keep_child_signals_blocked = if daemonize {
            KeepChildSignalsBlocked::Yes
        } else {
            // TODO: Keep signals blocked through exec preparation for non-daemon
            // children too. Restoring the inherited mask here can run application
            // handlers after a raw fork. This path currently has no non-test callers.
            KeepChildSignalsBlocked::No
        };
        let first_fork = unsafe { fork_skip_atfork_handlers(keep_child_signals_blocked) };
        match first_fork {
            RawFork::Parent(child_pid) => {
                temp_files.disarm();
                // temp_files is dropped here without unlinking its paths.
                return Ok(Child {
                    pid: child_pid,
                    status_read,
                });
            }
            RawFork::Error(error) => {
                // temp_files is dropped here and unlinks its paths.
                anyhow::bail!(std::io::Error::from_raw_os_error(error));
            }
            RawFork::Child => {}
        }

        // We're in the (first) child. We can't drop memory, and can't let
        // OwnedFds call libc close(). Current code would generally not do that
        // and instead calls _exit() directly before anything is dropped,
        // but let's be defensive against future changes.
        let mut argv = ManuallyDrop::new(argv);
        let temp_files = ManuallyDrop::new(temp_files);
        let envp = ManuallyDrop::new(envp);
        let fd_plan = ManuallyDrop::new(fd_plan);
        let prepared_spawn = ManuallyDrop::new(prepared_spawn);

        unsafe {
            ChildFdPlan::close_fd(status_read.into_raw_fd());
        }

        if daemonize {
            // A detached worker must not inherit the embedding runtime's signal
            // policy. Unlike execve, this also resets dispositions set to SIG_IGN.
            if unsafe { reset_signal_dispositions() }.is_err() {
                unsafe {
                    child_fail(
                        fd_plan.status_write.as_fd(),
                        temp_files.as_slice(),
                        ChildError::ResetSignalDispositions,
                    );
                }
            }
            // Start a new session and process group and drop the inherited
            // controlling terminal before the final fork.
            if unsafe { libc::setsid() } == -1 {
                unsafe {
                    child_fail(
                        fd_plan.status_write.as_fd(),
                        temp_files.as_slice(),
                        ChildError::SetSid,
                    );
                }
            }
        }

        // Apply the precomputed descriptor plan in the child.
        unsafe {
            fd_plan.apply(prepared_spawn.exec_fd(), temp_files.as_slice());
        }

        // Optionally detach through a second fork. Since setsid made the first
        // child a session leader, the grandchild cannot acquire a controlling
        // terminal implicitly.
        if daemonize {
            // avoid redudant restoration of signal mask in the grandchild,
            // since we clear it afterwards
            match unsafe { fork_skip_atfork_handlers(KeepChildSignalsBlocked::Yes) } {
                RawFork::Error(_) => unsafe {
                    child_fail(
                        fd_plan.status_write.as_fd(),
                        temp_files.as_slice(),
                        ChildError::DaemonFork,
                    );
                },
                RawFork::Parent(_) => unsafe {
                    // The grandchild owns the prepared artifacts. Exit without
                    // running Rust or libc destructors in the intermediate child.
                    libc::_exit(0);
                },
                RawFork::Child => {}
            }

            // we're the second child here

            if unsafe { clear_signal_mask() }.is_err() {
                unsafe {
                    child_fail(
                        fd_plan.status_write.as_fd(),
                        temp_files.as_slice(),
                        ChildError::ClearSignalMask,
                    );
                }
            }
        }

        // Replace the child with the prepared executable.
        prepared_spawn.exec(
            &mut argv,
            &envp,
            fd_plan.status_write.as_fd(),
            temp_files.as_slice(),
        );
    }
}

pub struct Child {
    pub pid: libc::pid_t,
    status_read: File,
}

impl Child {
    /// Wait for setup/exec error reporting to finish and reap the first child.
    ///
    /// # Returns
    ///
    /// If no setup/exec error is reported and `waitpid` succeeds, returns `Ok(status)`:
    ///
    /// - Without daemonization, `status` is the worker's wait status.
    /// - With daemonization, `status` is the intermediate child's wait status, normally
    ///   `WaitStatus::Exited(pid, 0)`. This waits for the status pipe to close, normally when the
    ///   detached worker execs, but does not wait for that worker to exit or return its eventual
    ///   exit status.
    ///
    /// A nonzero exit code or signal termination of the first child is returned
    /// as `Ok(WaitStatus::Exited(..))` or `Ok(WaitStatus::Signaled(..))` unless a
    /// setup/exec error was also reported. [`SpawnWorker::wait_spawn`] additionally
    /// treats these unsuccessful wait statuses as errors.
    ///
    /// # Errors
    ///
    /// Returns an error if either child reports a failure to configure signals,
    /// create a session, duplicate or close descriptors, perform the daemonizing
    /// fork, or exec. Also returns an error if reading the status pipe fails,
    /// the pipe contains an unknown error code, or `waitpid` fails (for example,
    /// because the first child has already been reaped).
    ///
    /// Reaping is attempted even when the status pipe reports an error. An error
    /// reported or encountered on the pipe takes precedence over a `waitpid` error.
    pub fn wait(self) -> anyhow::Result<WaitStatus> {
        let child_status = read_child_status(self.status_read);
        let pid = Pid::from_raw(self.pid);

        // Always reap the first child, including when the status read failed.
        let wait_status = loop {
            match nix::sys::wait::waitpid(Some(pid), None) {
                Err(nix::errno::Errno::EINTR) => continue,
                result => break result,
            }
        };
        child_status?;
        Ok(wait_status?)
    }

    fn wait_success(self) -> anyhow::Result<()> {
        match self.wait()? {
            WaitStatus::Exited(_, 0) => Ok(()),
            status => anyhow::bail!("spawn_worker: child failed: {status:?}"),
        }
    }
}

enum ChildStdio {
    Inherit,
    Owned(OwnedFd),
    Ref(libc::pid_t),
}

impl ChildStdio {
    fn as_fd(&self) -> Option<RawFd> {
        match self {
            ChildStdio::Inherit => None,
            ChildStdio::Owned(fd) => Some(fd.as_raw_fd()),
            ChildStdio::Ref(fd) => Some(*fd),
        }
    }
}

/// Descriptor setup to apply in the child: map standard streams to fds 0-2,
/// the optional passed descriptor to fd 3, and dependencies to the following
/// fds. Sources that could be overwritten are protected before the fork; after
/// the mappings are installed, the plan preserves the status pipe and a required
/// exec fd and tries to close higher fds using Linux close_range or exact
/// descriptor-directory enumeration.
pub(crate) struct ChildFdPlan<'fd> {
    stdio: [ChildStdio; 3],
    mappings: Vec<FdMapping<'fd>>,
    source_fd_minimum: RawFd,
    first_to_close: u32,
    status_write: OwnedFd,
    directory_fd_reservation: Option<OwnedFd>,
}

struct FdMapping<'fd> {
    source: MaybeOwnedFd<'fd>,
    destination: RawFd,
}

enum MaybeOwnedFd<'fd> {
    Owned(OwnedFd),
    Borrowed(BorrowedFd<'fd>),
}
impl AsRawFd for MaybeOwnedFd<'_> {
    fn as_raw_fd(&self) -> RawFd {
        match self {
            MaybeOwnedFd::Owned(fd) => fd.as_raw_fd(),
            MaybeOwnedFd::Borrowed(fd) => fd.as_raw_fd(),
        }
    }
}

impl<'fd> ChildFdPlan<'fd> {
    /// Build the complete descriptor plan and return its status reader.
    /// The plan owns the writer. Must be called before the fork.
    pub(crate) fn new<I>(
        stdin: &Stdio,
        stdout: &Stdio,
        stderr: &Stdio,
        passed_fd: Option<BorrowedFd<'fd>>,
        dependency_fds: I,
    ) -> anyhow::Result<(Self, File)>
    where
        I: ExactSizeIterator<Item = OwnedFd>,
    {
        let has_passed_fd = passed_fd.is_some();
        let dependency_count = dependency_fds.len();
        let dependency_count_fd = RawFd::try_from(dependency_count)?;
        let dependency_destination = Self::dependency_destination(has_passed_fd);
        let last_destination = if dependency_count_fd > 0 {
            dependency_destination + dependency_count_fd - 1
        } else if has_passed_fd {
            3
        } else {
            2
        };
        let source_fd_minimum = last_destination
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("too many child file descriptors"))?;
        let (status_read, status_write) = Self::child_status_pipe(source_fd_minimum)?;

        let mut plan = Self {
            stdio: [
                stdin.as_child_stdio()?,
                stdout.as_child_stdio()?,
                stderr.as_child_stdio()?,
            ],
            mappings: Vec::with_capacity(usize::from(has_passed_fd) + dependency_count),
            source_fd_minimum,
            first_to_close: u32::try_from(source_fd_minimum)?,
            status_write,
            directory_fd_reservation: None,
        };

        if let Some(fd) = passed_fd {
            plan.add_borrowed_mapping(fd, 3)?;
        }
        for (index, fd) in dependency_fds.enumerate() {
            plan.add_owned_mapping(fd, dependency_destination + RawFd::try_from(index)?)?;
        }

        Ok((plan, status_read))
    }

    /// Determine the dependency layout while preparing arguments before the fork.
    pub(crate) fn dependency_destination(has_passed_fd: bool) -> RawFd {
        if has_passed_fd {
            4
        } else {
            3
        }
    }

    /// Protect a descriptor from later remapping. Must be called before the fork.
    pub(crate) fn duplicate_source(&self, fd: RawFd) -> std::io::Result<OwnedFd> {
        Self::duplicate_fd_at_least(fd, self.source_fd_minimum)
    }

    /// Create the status channel before forking. The writer is at least
    /// first_to_close, above all remapping destinations. Both ends are CLOEXEC.
    fn child_status_pipe(source_fd_minimum: RawFd) -> std::io::Result<(File, OwnedFd)> {
        #[cfg(target_os = "linux")]
        let (read, write) = nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC)?;
        #[cfg(target_os = "macos")]
        let (read, write) = {
            let (read, write) = nix::unistd::pipe()?;
            for fd in [&read, &write] {
                nix::fcntl::fcntl(
                    fd.as_raw_fd(),
                    nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC),
                )?;
            }
            (read, write)
        };
        let writer = nix::fcntl::fcntl(
            write.as_raw_fd(),
            nix::fcntl::FcntlArg::F_DUPFD_CLOEXEC(source_fd_minimum),
        )?;
        // SAFETY: F_DUPFD_CLOEXEC returned a new descriptor owned by this function.
        Ok((File::from(read), unsafe { OwnedFd::from_raw_fd(writer) }))
    }

    /// Reserve a descriptor for child-side directory enumeration. Must be
    /// called after preparing all other descriptors and before the fork.
    /// The reservation is placed above all remapping destinations
    pub(crate) fn reserve_directory_fd(&mut self) -> std::io::Result<()> {
        let reservation = OwnedFd::from(File::open("/dev/null")?);
        let reservation = if reservation.as_raw_fd() >= self.source_fd_minimum {
            reservation
        } else {
            self.duplicate_source(reservation.as_raw_fd())?
        };
        self.directory_fd_reservation = Some(reservation);
        Ok(())
    }

    /// Apply the plan after the fork. Must be called only in the child.
    /// Both preserved descriptors must be above all remapping destinations;
    /// their relative order does not matter.
    pub(crate) unsafe fn apply(&self, exec_fd: Option<RawFd>, temp_files: &[CString]) {
        let status_fd = self.status_write.as_fd();
        for (child_stdio, destination) in self.stdio.iter().zip(0..=libc::STDERR_FILENO) {
            if let Some(source) = child_stdio.as_fd() {
                if unsafe { libc::dup2(source, destination) } == -1 {
                    unsafe {
                        child_fail(status_fd, temp_files, ChildError::DupFd);
                    }
                }
            }
        }

        for mapping in &self.mappings {
            if unsafe { libc::dup2(mapping.source.as_raw_fd(), mapping.destination) } == -1 {
                unsafe {
                    child_fail(status_fd, temp_files, ChildError::DupFd);
                }
            }
        }

        // Release the slot reserved in the parent immediately before opening
        // the descriptor directory. The reservation is above every mapping
        // destination, so none of the dup2 calls above can overwrite it.
        if let Some(reservation) = &self.directory_fd_reservation {
            unsafe {
                Self::close_fd(reservation.as_raw_fd());
            }
        }

        // Close everything above first_to_close... except for exec_fd and status_fd
        // Both descriptors were allocated above all remapping destinations in
        // the parent. Valid descriptors are nonnegative, so these casts are safe.
        let status_fd_number = status_fd.as_raw_fd() as u32;
        let closed_descriptors = match exec_fd {
            None => unsafe {
                Self::close_fd_range(self.first_to_close, status_fd_number.saturating_sub(1))
                    && Self::close_fd_range(status_fd_number.saturating_add(1), u32::MAX)
            },
            Some(exec_fd) => {
                let exec_fd_number = exec_fd as u32;
                let lower_fd = status_fd_number.min(exec_fd_number);
                let higher_fd = status_fd_number.max(exec_fd_number);
                unsafe {
                    Self::close_fd_range(self.first_to_close, lower_fd.saturating_sub(1))
                        && Self::close_fd_range(
                            lower_fd.saturating_add(1),
                            higher_fd.saturating_sub(1),
                        )
                        && Self::close_fd_range(higher_fd.saturating_add(1), u32::MAX)
                }
            }
        };
        if !closed_descriptors {
            unsafe {
                child_fail(status_fd, temp_files, ChildError::CloseFds);
            }
        }
    }

    fn add_borrowed_mapping(
        &mut self,
        source: BorrowedFd<'fd>,
        destination: RawFd,
    ) -> std::io::Result<()> {
        let source = if Self::source_is_safe_without_duplication(source.as_raw_fd(), destination) {
            MaybeOwnedFd::Borrowed(source)
        } else {
            MaybeOwnedFd::Owned(self.duplicate_source(source.as_raw_fd())?)
        };
        self.mappings.push(FdMapping {
            source,
            destination,
        });
        Ok(())
    }

    fn add_owned_mapping(&mut self, source: OwnedFd, destination: RawFd) -> std::io::Result<()> {
        let source = if Self::source_is_safe_without_duplication(source.as_raw_fd(), destination) {
            // Retain ownership so the descriptor remains open until the plan
            // has installed it in the child.
            MaybeOwnedFd::Owned(source)
        } else {
            MaybeOwnedFd::Owned(self.duplicate_source(source.as_raw_fd())?)
        };
        self.mappings.push(FdMapping {
            source,
            destination,
        });
        Ok(())
    }

    fn source_is_safe_without_duplication(source: RawFd, destination: RawFd) -> bool {
        // Destinations are installed in increasing order, so a greater-numbered
        // source cannot be overwritten before it is used. Equality is unsafe:
        // dup2 would be a no-op and would not clear FD_CLOEXEC.
        source > destination
    }

    fn duplicate_fd_at_least(fd: RawFd, minimum: RawFd) -> std::io::Result<OwnedFd> {
        let duplicated = nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_DUPFD(minimum))?;
        // SAFETY: F_DUPFD returned a new descriptor owned by this function.
        Ok(unsafe { OwnedFd::from_raw_fd(duplicated) })
    }

    #[cfg(target_os = "linux")]
    unsafe fn close_fd_range(first: u32, last: u32) -> bool {
        if first > last {
            return true;
        }
        if unsafe { libc::syscall(libc::SYS_close_range, first, last, 0) } == 0 {
            return true;
        }
        unsafe { Self::close_open_fds_from_proc(first, last) }
    }

    #[cfg(target_os = "macos")]
    unsafe fn close_fd_range(first: u32, last: u32) -> bool {
        if first > last {
            return true;
        }
        unsafe { Self::close_open_fds_from_dev(first, last) }
    }

    #[cfg(target_os = "linux")]
    unsafe fn close_open_fds_from_proc(first: u32, last: u32) -> bool {
        unsafe { Self::enumerate_and_close_open_fds_from_proc(first, last) }.is_some()
    }

    #[cfg(target_os = "linux")]
    unsafe fn enumerate_and_close_open_fds_from_proc(first: u32, last: u32) -> Option<usize> {
        // linux_dirent64 has fixed-width fields through d_type, followed by the
        // variable-length, NUL-terminated d_name at byte 19.
        const DIRENT64_NAME_OFFSET: usize = 19;
        const DIRENT64_RECLEN_OFFSET: usize = 16;
        const BUFFER_SIZE: usize = 4096;

        let directory_fd = unsafe {
            libc::syscall(
                libc::SYS_openat,
                libc::AT_FDCWD,
                c"/proc/self/fd".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                0,
            )
        };
        if directory_fd < 0 {
            return None;
        }
        let Ok(directory_fd) = RawFd::try_from(directory_fd) else {
            return None;
        };

        // Both glibc and musl allocate in opendir(), and their allocator locks
        // are not repaired after _Fork or a raw fork. Use raw syscalls and fixed
        // stack storage so the child cannot deadlock on inherited libc state.
        // The child is single-threaded with signals blocked, so its descriptor
        // table cannot change except through the syscalls in this function.
        let mut buffer = [0_u8; BUFFER_SIZE];
        let mut batch_count = 0;
        let enumerated_all = 'read_entries: loop {
            let bytes_read = unsafe {
                libc::syscall(
                    libc::SYS_getdents64,
                    directory_fd,
                    buffer.as_mut_ptr(),
                    buffer.len(),
                )
            };
            if bytes_read == 0 {
                break true;
            }
            let Ok(bytes_read) = usize::try_from(bytes_read) else {
                break false;
            };
            if bytes_read > buffer.len() {
                break false;
            }
            batch_count += 1;

            let mut offset = 0;
            while offset < bytes_read {
                let remaining = bytes_read - offset;
                if remaining <= DIRENT64_NAME_OFFSET {
                    break 'read_entries false;
                }
                let Some(&[reclen_low, reclen_high]) = buffer
                    .get(offset + DIRENT64_RECLEN_OFFSET..offset + DIRENT64_RECLEN_OFFSET + 2)
                else {
                    break 'read_entries false;
                };
                let record_length = usize::from(u16::from_ne_bytes([reclen_low, reclen_high]));
                if record_length <= DIRENT64_NAME_OFFSET || record_length > remaining {
                    break 'read_entries false;
                }
                let name_offset = offset + DIRENT64_NAME_OFFSET;
                let record_end = offset + record_length;
                let Some(name) = buffer.get(name_offset..record_end) else {
                    break 'read_entries false;
                };

                if let Some(fd) = Self::parse_fd_name(name) {
                    let Ok(fd_number) = u32::try_from(fd) else {
                        break 'read_entries false;
                    };
                    if fd != directory_fd && fd_number >= first && fd_number <= last {
                        unsafe {
                            Self::close_fd(fd);
                        }
                    }
                }
                offset = record_end;
            }
        };

        unsafe {
            Self::close_fd(directory_fd);
        }
        enumerated_all.then_some(batch_count)
    }

    #[cfg(target_os = "macos")]
    unsafe fn close_open_fds_from_dev(first: u32, last: u32) -> bool {
        // macOS 12+ implements vfork with fork-like address-space semantics and
        // runs LibSystem's internal malloc and pthread repair hooks. Using these
        // allocating, locking APIs deliberately relies on that stock LibSystem
        // behavior; they are not generally async-signal-safe after a POSIX fork.
        let directory = unsafe { libc::opendir(c"/dev/fd".as_ptr()) };
        if directory.is_null() {
            return false;
        }
        let directory_fd = unsafe { libc::dirfd(directory) };
        if directory_fd == -1 {
            unsafe {
                libc::closedir(directory);
            }
            return false;
        }

        let enumerated_all = loop {
            unsafe {
                *libc::__error() = 0;
            }
            let entry = unsafe { libc::readdir(directory) };
            if entry.is_null() {
                break unsafe { *libc::__error() } == 0;
            }

            let name = unsafe { ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if let Some(fd) = Self::parse_fd_name(name) {
                let Ok(fd_number) = u32::try_from(fd) else {
                    break false;
                };
                if fd != directory_fd && fd_number >= first && fd_number <= last {
                    unsafe {
                        Self::close_fd(fd);
                    }
                }
            }
        };

        let closed_directory = unsafe { libc::closedir(directory) } == 0;
        enumerated_all && closed_directory
    }

    fn parse_fd_name(name: &[u8]) -> Option<RawFd> {
        let mut value = 0_u32;
        let mut has_digit = false;
        for byte in name {
            if *byte == 0 {
                break;
            }
            if !byte.is_ascii_digit() {
                return None;
            }
            has_digit = true;
            value = value
                .checked_mul(10)?
                .checked_add(u32::from(*byte - b'0'))?;
        }
        if has_digit {
            RawFd::try_from(value).ok()
        } else {
            None
        }
    }

    #[cfg(target_os = "linux")]
    unsafe fn close_fd(fd: RawFd) {
        // Avoid musl's AIO- and cancellation-aware close wrapper in a child
        // whose libc thread state may not have been repaired after a raw fork.
        unsafe {
            libc::syscall(libc::SYS_close, fd);
        }
    }

    #[cfg(not(target_os = "linux"))]
    unsafe fn close_fd(fd: RawFd) {
        unsafe {
            libc::close(fd);
        }
    }
}

impl Stdio {
    fn as_child_stdio(&self) -> std::io::Result<ChildStdio> {
        match self {
            Stdio::Inherit => Ok(ChildStdio::Inherit),
            Stdio::Fd(fd) => {
                if fd.as_raw_fd() >= 0 && fd.as_raw_fd() <= libc::STDERR_FILENO {
                    Ok(ChildStdio::Owned(fd.try_clone()?))
                } else {
                    Ok(ChildStdio::Ref(fd.as_raw_fd()))
                }
            }
            Stdio::Null => {
                let dev_null = File::options().read(true).write(true).open("/dev/null")?;
                Ok(ChildStdio::Owned(dev_null.into()))
            }
        }
    }
}

struct TempFiles {
    paths: Vec<CString>,
    cleanup_on_drop: bool,
}

impl TempFiles {
    fn new() -> Self {
        Self {
            paths: Vec::new(),
            cleanup_on_drop: true,
        }
    }

    fn persist(&mut self, data: &[u8]) -> anyhow::Result<CString> {
        let tmp_file = tempfile::NamedTempFile::new()?;
        let mut file = tmp_file.as_file();
        file.set_len(data.len() as u64)?;
        file.write_all(data)?;
        file.rewind()?;

        std::fs::set_permissions(tmp_file.path(), Permissions::from_mode(0o700))?;

        let path = CString::new(tmp_file.path().as_os_str().as_bytes())?;
        tmp_file.into_temp_path().keep()?;
        self.paths.push(path.clone());
        Ok(path)
    }

    fn as_slice(&self) -> &[CString] {
        &self.paths
    }

    fn disarm(&mut self) {
        self.cleanup_on_drop = false;
    }
}

impl Drop for TempFiles {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            for path in &self.paths {
                unsafe {
                    libc::unlink(path.as_ptr());
                }
            }
        }
    }
}

enum PreparedSpawn {
    #[cfg(target_os = "linux")]
    Direct(CString),
    #[cfg(target_os = "linux")]
    FdExec {
        fd: OwnedFd,
        fallback_path: Option<CString>,
    },
    #[cfg(not(target_os = "macos"))]
    LdPreload(CString),
    Exec(CString),
}

impl PreparedSpawn {
    fn exec(
        &self,
        argv: &mut ExecVec,
        envp: &ExecVec,
        status_fd: BorrowedFd<'_>,
        temp_files: &[CString],
    ) -> ! {
        unsafe {
            match self {
                #[cfg(target_os = "linux")]
                Self::Direct(path) => {
                    libc::execve(path.as_ptr(), argv.as_ptr(), envp.as_ptr());
                }
                #[cfg(target_os = "linux")]
                Self::FdExec { fd, fallback_path } => {
                    // Avoid fexecve because older glibc versions format a
                    // /proc/self/fd path after fork. If raw execveat fails,
                    // materialize the prepared disk fallback using raw syscalls.
                    libc::syscall(
                        libc::SYS_execveat,
                        fd.as_raw_fd(),
                        c"".as_ptr(),
                        argv.as_ptr(),
                        envp.as_ptr(),
                        libc::AT_EMPTY_PATH,
                    );
                    if let Some(fallback_path) = fallback_path {
                        if linux::materialize_fallback(fallback_path) {
                            // SAFETY: FdExec always reserves argv[1], and fallback_path belongs
                            // to PreparedSpawn and remains alive until exec or _exit.
                            argv.set_borrowed(1, fallback_path);
                            libc::syscall(
                                libc::SYS_execve,
                                fallback_path.as_ptr(),
                                argv.as_ptr(),
                                envp.as_ptr(),
                            );
                            linux::unlink(fallback_path);
                        }
                    }
                }
                #[cfg(not(target_os = "macos"))]
                Self::LdPreload(path) | Self::Exec(path) => {
                    libc::execve(path.as_ptr(), argv.as_ptr(), envp.as_ptr());
                }
                #[cfg(target_os = "macos")]
                Self::Exec(path) => {
                    libc::execve(path.as_ptr(), argv.as_ptr(), envp.as_ptr());
                }
            }
            child_fail(status_fd, temp_files, ChildError::Exec)
            // in the successful path, it's the trampoline that unlinks the temp files
        }
    }

    fn exec_fd(&self) -> Option<RawFd> {
        match self {
            #[cfg(target_os = "linux")]
            Self::FdExec { fd, .. } => Some(fd.as_raw_fd()),
            _ => None,
        }
    }
}

fn read_child_status(mut read: File) -> anyhow::Result<()> {
    let mut code = [0];
    // A single byte cannot be partially read; read_exact retries EINTR.
    match read.read_exact(&mut code) {
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(()),
        Err(error) => Err(error.into()),
        Ok(()) => match ChildError::from_code(code[0]) {
            Some(error) => Err(error.into()),
            None => anyhow::bail!("spawn_worker: invalid child status {}", code[0]),
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum ChildError {
    ResetSignalDispositions = 1,
    SetSid,
    DupFd,
    CloseFds,
    DaemonFork,
    ClearSignalMask,
    Exec,
}

impl ChildError {
    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::ResetSignalDispositions),
            2 => Some(Self::SetSid),
            3 => Some(Self::DupFd),
            4 => Some(Self::CloseFds),
            5 => Some(Self::DaemonFork),
            6 => Some(Self::ClearSignalMask),
            7 => Some(Self::Exec),
            _ => None,
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::ResetSignalDispositions => "spawn_worker: resetting signal dispositions failed\n",
            Self::SetSid => "spawn_worker: setsid failed\n",
            Self::DupFd => "spawn_worker: dup2 failed\n",
            Self::CloseFds => "spawn_worker: closing file descriptors failed\n",
            Self::DaemonFork => "spawn_worker: daemon fork failed\n",
            Self::ClearSignalMask => "spawn_worker: clearing signal mask failed\n",
            Self::Exec => "spawn_worker: exec failed\n",
        }
    }
}

impl std::fmt::Display for ChildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message().trim_end())
    }
}

impl std::error::Error for ChildError {}

unsafe fn child_fail(status_fd: BorrowedFd<'_>, temp_files: &[CString], error: ChildError) -> ! {
    // Clean up first: dropping Child without waiting closes the reader, so
    // reporting the error may terminate this child with SIGPIPE.
    for temp_file in temp_files {
        unsafe {
            // unlink() is async-signal-safe
            libc::unlink(temp_file.as_ptr());
        }
    }
    // The repr(u8) discriminant fits in one byte. Report before stderr writes,
    // which may fail or block. No allocation or formatting here.
    let code = [error as u8];
    #[cfg(target_os = "linux")]
    linux::write_all(status_fd.as_raw_fd(), &code);
    #[cfg(target_os = "macos")]
    loop {
        let result =
            unsafe { libc::write(status_fd.as_raw_fd(), code.as_ptr().cast(), code.len()) };
        if result != -1 || unsafe { *libc::__error() } != libc::EINTR {
            break;
        }
    }

    let message = error.message().as_bytes();
    unsafe {
        #[cfg(target_os = "linux")]
        linux::write_all(libc::STDERR_FILENO, message);
        #[cfg(target_os = "macos")]
        libc::write(
            libc::STDERR_FILENO,
            message.as_ptr().cast::<libc::c_void>(),
            message.len(),
        );
        libc::_exit(1);
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        env,
        ffi::{CStr, CString},
        io::{Seek, Write},
        os::unix::ffi::OsStrExt,
    };

    use nix::libc;

    use crate::TRAMPOLINE_BIN;

    /// Prepare a random fallback name before forking without creating the file.
    pub(crate) fn prepare_fallback_path() -> anyhow::Result<CString> {
        const PREFIX: &[u8] = b"dd-ipc-spawn_";
        const HEX: &[u8; 16] = b"0123456789abcdef";

        let mut random = [0_u8; 16];
        getrandom::fill(&mut random)
            .map_err(|error| anyhow::format_err!("cannot prepare fallback name: {error}"))?;

        let mut directory = env::temp_dir();
        if !directory.is_absolute() {
            directory = env::current_dir()?.join(directory);
        }
        let directory = directory.as_os_str().as_bytes();
        let needs_separator = !directory.ends_with(b"/");
        let mut path = Vec::with_capacity(
            directory.len() + usize::from(needs_separator) + PREFIX.len() + random.len() * 2,
        );
        path.extend_from_slice(directory);
        if needs_separator {
            path.push(b'/');
        }
        path.extend_from_slice(PREFIX);
        for byte in random {
            path.push(HEX[usize::from(byte >> 4)]);
            path.push(HEX[usize::from(byte & 0x0f)]);
        }

        Ok(CString::new(path)?)
    }

    pub(crate) fn write_trampoline() -> anyhow::Result<memfd::Memfd> {
        write_memfd("spawn_worker_trampoline", TRAMPOLINE_BIN)
    }

    pub(crate) fn write_memfd(name: &str, contents: &[u8]) -> anyhow::Result<memfd::Memfd> {
        // This leaks a fd, but a fd to the TXT segment, which is fine.
        // And it will ensure that fexecve works with custom binfmts (rosetta or qemu).
        let opts = memfd::MemfdOptions::default().close_on_exec(false);
        let mfd = opts.create(name)?;

        mfd.as_file().set_len(contents.len() as u64)?;
        mfd.as_file().write_all(contents)?;
        mfd.as_file().rewind()?;

        Ok(mfd)
    }

    /// Create and populate the disk fallback after forking using raw syscall
    /// wrappers and preallocated storage.
    pub(super) fn materialize_fallback(path: &CStr) -> bool {
        let flags =
            libc::O_CLOEXEC | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_WRONLY;
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat,
                libc::AT_FDCWD,
                path.as_ptr(),
                flags,
                0o600 as libc::mode_t,
            )
        } as libc::c_int;
        if fd < 0 {
            return false;
        }

        let materialized = write_all(fd, TRAMPOLINE_BIN)
            && unsafe { libc::syscall(libc::SYS_fchmod, fd, 0o700 as libc::mode_t) } == 0;

        // On Linux the descriptor is released even when close reports an error.
        unsafe {
            libc::syscall(libc::SYS_close, fd);
        }
        if !materialized {
            unlink(path);
        }
        materialized
    }

    pub(super) fn unlink(path: &CStr) {
        unsafe {
            libc::syscall(libc::SYS_unlinkat, libc::AT_FDCWD, path.as_ptr(), 0);
        }
    }

    pub(super) fn write_all(fd: libc::c_int, contents: &[u8]) -> bool {
        let mut offset = 0;
        while offset < contents.len() {
            let result = unsafe {
                libc::syscall(
                    libc::SYS_write,
                    fd,
                    contents.as_ptr().add(offset),
                    contents.len() - offset,
                )
            };
            if result > 0 && result as usize <= contents.len() - offset {
                offset += result as usize;
            } else if result == -1 && unsafe { *libc::__errno_location() } == libc::EINTR {
                continue;
            } else {
                return false;
            }
        }
        true
    }
}

mod helper {
    use nix::libc;
    use std::{ffi::CString, ptr};

    pub struct ExecVec {
        items: Vec<CString>,
        // Always NULL ptr terminated
        ptrs: Vec<*const libc::c_char>,
    }

    impl ExecVec {
        pub fn as_ptr(&self) -> *const *const libc::c_char {
            self.ptrs.as_ptr()
        }

        pub fn empty() -> Self {
            Self {
                items: vec![],
                ptrs: vec![ptr::null()],
            }
        }

        pub fn push(&mut self, item: CString) {
            let l = self.ptrs.len();
            // replace previous trailing null with ptr to the item
            self.ptrs[l - 1] = item.as_ptr();
            self.ptrs.push(ptr::null());
            self.items.push(item);
        }

        pub fn set(&mut self, index: usize, item: CString) {
            self.ptrs[index] = item.as_ptr();
            self.items[index] = item;
        }

        /// Point an existing argument slot at storage owned elsewhere without
        /// allocating or freeing memory.
        ///
        /// # Safety
        ///
        /// The index must name an existing argument, and the item must remain
        /// alive until this vector is no longer used.
        #[cfg(target_os = "linux")]
        pub unsafe fn set_borrowed(&mut self, index: usize, item: &std::ffi::CStr) {
            unsafe {
                *self.ptrs.get_unchecked_mut(index) = item.as_ptr();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn wait_success_reports_child_failure() {
        match unsafe { fork_skip_atfork_handlers(KeepChildSignalsBlocked::Yes) } {
            RawFork::Child => unsafe { libc::_exit(1) },
            RawFork::Parent(pid) => {
                let child = Child {
                    pid,
                    status_read: File::open("/dev/null").unwrap(),
                };
                assert!(child.wait_success().is_err());
            }
            RawFork::Error(error) => panic!("fork failed with errno {error}"),
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn wait_spawn_reports_daemon_exec_failure() {
        // Exceed ARG_MAX on macOS and Linux without changing the parent's environment.
        let error = SpawnWorker::from_env(
            test_target(),
            [(
                OsString::from("TOO_LARGE"),
                "x".repeat(3 * 1024 * 1024).into(),
            )],
        )
        .spawn_method(SpawnMethod::Exec)
        .daemonize(true)
        .stderr(Stdio::Null)
        .wait_spawn();
        assert_eq!(Some(&ChildError::Exec), error.unwrap_err().downcast_ref());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn wait_reports_first_child_setup_failure_and_reaps_child() {
        for daemonize in [false, true] {
            let mut worker = SpawnWorker::from_env(test_target(), []);
            worker.daemonize(daemonize).stderr(Stdio::Null);
            let (mut fd_plan, status_read) = ChildFdPlan::new(
                &worker.stdin,
                &worker.stdout,
                &worker.stderr,
                None,
                std::iter::empty(),
            )
            .unwrap();
            // Force a dup2 failure after the first fork, before daemonization's
            // second fork, without invalidating any parent-owned descriptor.
            fd_plan.stdio[0] = ChildStdio::Ref(-1);
            let mut temp_files = TempFiles::new();
            let path = temp_files.persist(b"cleanup after setup failure").unwrap();
            let child = worker
                .spawn_prepared(
                    fd_plan,
                    status_read,
                    PreparedSpawn::Exec(c"/bin/true".into()),
                    ExecVec::empty(),
                    ExecVec::empty(),
                    temp_files,
                )
                .unwrap();
            let pid = child.pid;
            let error = child.wait().unwrap_err();
            assert_eq!(Some(&ChildError::DupFd), error.downcast_ref());
            assert_child_reaped(pid);
            assert!(!std::path::Path::new(ffi::OsStr::from_bytes(path.as_bytes())).exists());
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn wait_reports_exec_failure_and_reaps_child_with_and_without_daemonization() {
        for daemonize in [false, true] {
            let mut worker = SpawnWorker::from_env(test_target(), []);
            worker.daemonize(daemonize).stderr(Stdio::Null);
            let (fd_plan, status_read) = ChildFdPlan::new(
                &worker.stdin,
                &worker.stdout,
                &worker.stderr,
                None,
                std::iter::empty(),
            )
            .unwrap();
            let child = worker
                .spawn_prepared(
                    fd_plan,
                    status_read,
                    PreparedSpawn::Exec(c"/spawn_worker/path/that/does/not/exist".into()),
                    ExecVec::empty(),
                    ExecVec::empty(),
                    TempFiles::new(),
                )
                .unwrap();
            let pid = child.pid;
            let error = child.wait().unwrap_err();
            assert_eq!(Some(&ChildError::Exec), error.downcast_ref());
            assert_child_reaped(pid);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn spawn_returns_without_waiting_for_worker_exit() {
        for daemonize in [false, true] {
            let (gate_read, gate_write) = nix::unistd::pipe().unwrap();
            let (output_read, output_write) = nix::unistd::pipe().unwrap();
            let mut worker = SpawnWorker::from_env(test_target(), []);
            worker
                .daemonize(daemonize)
                .pass_fd(gate_read)
                .stdout(Stdio::Fd(output_write));
            let (fd_plan, status_read) = ChildFdPlan::new(
                &worker.stdin,
                &worker.stdout,
                &worker.stderr,
                worker.fd_to_pass.as_ref().map(AsFd::as_fd),
                std::iter::empty(),
            )
            .unwrap();
            let mut argv = ExecVec::empty();
            for arg in [c"sh", c"-c", c"read value <&3; printf started"] {
                argv.push(arg.into());
            }
            let child = worker
                .spawn_prepared(
                    fd_plan,
                    status_read,
                    PreparedSpawn::Exec(c"/bin/sh".into()),
                    argv,
                    ExecVec::empty(),
                    TempFiles::new(),
                )
                .unwrap();
            // The worker cannot exit until this write.
            File::from(gate_write).write_all(b"continue\n").unwrap();
            child.wait_success().unwrap();
            drop(worker);
            let mut output = String::new();
            File::from(output_read).read_to_string(&mut output).unwrap();
            assert_eq!("started", output);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn spawn_returns_before_status_pipe_eof() {
        for daemonize in [false, true] {
            let (fd_plan, status_read) = empty_fd_plan();
            // Keep the pipe open regardless of how quickly the child execs or
            // exits. Spawn must return while this additional writer is held.
            let writer = fd_plan.status_write.try_clone().unwrap();
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            let spawning = std::thread::spawn(move || {
                let mut worker = SpawnWorker::from_env(test_target(), []);
                worker.daemonize(daemonize);
                let mut argv = ExecVec::empty();
                for arg in [c"sh", c"-c", c"exit 0"] {
                    argv.push(arg.into());
                }
                let result = worker.spawn_prepared(
                    fd_plan,
                    status_read,
                    PreparedSpawn::Exec(c"/bin/sh".into()),
                    argv,
                    ExecVec::empty(),
                    TempFiles::new(),
                );
                assert!(sender.send(result).is_ok());
            });
            let result = receiver.recv_timeout(std::time::Duration::from_secs(5));
            // Release the writer even on timeout so a blocking regression can
            // finish, be reaped, and fail the test without leaving a thread.
            drop(writer);
            spawning.join().unwrap();
            match result {
                Ok(child) => child.unwrap().wait_success().unwrap(),
                Err(error) => {
                    receiver.recv().unwrap().unwrap().wait_success().unwrap();
                    panic!("spawn waited for status-pipe EOF: {error}");
                }
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn wait_reaps_child_when_status_read_fails() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let status_read = File::options().write(true).open(file.path()).unwrap();
        match unsafe { fork_skip_atfork_handlers(KeepChildSignalsBlocked::Yes) } {
            RawFork::Child => unsafe { libc::_exit(0) },
            RawFork::Parent(pid) => {
                let error = Child { pid, status_read }.wait().unwrap_err();
                assert_eq!(
                    Some(libc::EBADF),
                    error
                        .downcast_ref::<std::io::Error>()
                        .unwrap()
                        .raw_os_error(),
                );
                assert_child_reaped(pid);
            }
            RawFork::Error(error) => panic!("fork failed with errno {error}"),
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn child_failure_cleans_up_after_status_reader_is_dropped() {
        let (fd_plan, status_read) = empty_fd_plan();
        drop(status_read);
        let mut temp_files = TempFiles::new();
        let path = temp_files
            .persist(b"cleanup without a status reader")
            .unwrap();
        match unsafe { fork_skip_atfork_handlers(KeepChildSignalsBlocked::Yes) } {
            RawFork::Child => unsafe {
                if reset_signal_dispositions().is_err() || clear_signal_mask().is_err() {
                    libc::_exit(2);
                }
                child_fail(
                    fd_plan.status_write.as_fd(),
                    temp_files.as_slice(),
                    ChildError::Exec,
                );
            },
            RawFork::Parent(pid) => {
                Child {
                    pid,
                    status_read: File::open("/dev/null").unwrap(),
                }
                .wait()
                .unwrap();
                assert!(!std::path::Path::new(ffi::OsStr::from_bytes(path.as_bytes())).exists());
            }
            RawFork::Error(error) => panic!("fork failed with errno {error}"),
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn wait_success_reports_signal_death() {
        match unsafe { fork_skip_atfork_handlers(KeepChildSignalsBlocked::Yes) } {
            RawFork::Child => unsafe {
                #[cfg(target_os = "linux")]
                let pid = {
                    // Older glibc caches the parent's PID across a raw fork.
                    // SYS_getpid always succeeds and returns a value that fits pid_t.
                    libc::syscall(libc::SYS_getpid) as libc::pid_t
                };
                #[cfg(not(target_os = "linux"))]
                let pid = libc::getpid();
                libc::kill(pid, libc::SIGKILL);
                libc::_exit(1);
            },
            RawFork::Parent(pid) => {
                let error = Child {
                    pid,
                    status_read: File::open("/dev/null").unwrap(),
                }
                .wait_success()
                .unwrap_err();
                assert!(error.to_string().contains("SIGKILL"));
            }
            RawFork::Error(error) => panic!("fork failed with errno {error}"),
        }
    }

    #[test]
    fn fd_names_are_parsed_without_allocating() {
        assert_eq!(Some(0), ChildFdPlan::parse_fd_name(b"0\0"));
        assert_eq!(Some(123), ChildFdPlan::parse_fd_name(b"123\0padding"));
        assert_eq!(Some(123), ChildFdPlan::parse_fd_name(b"123"));
        assert_eq!(None, ChildFdPlan::parse_fd_name(b".\0"));
        assert_eq!(None, ChildFdPlan::parse_fd_name(b"12x\0"));
        assert_eq!(None, ChildFdPlan::parse_fd_name(b"2147483648\0"));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn descriptor_directory_walker_closes_only_descriptors_in_range() {
        let file = File::open("/dev/null").unwrap();
        let targets = (0..64)
            .map(|_| ChildFdPlan::duplicate_fd_at_least(file.as_raw_fd(), 64).unwrap())
            .collect::<Vec<_>>();
        let first_target_fd = targets.iter().map(AsRawFd::as_raw_fd).min().unwrap();
        let last_target_fd = targets.iter().map(AsRawFd::as_raw_fd).max().unwrap();
        let preserved =
            ChildFdPlan::duplicate_fd_at_least(file.as_raw_fd(), last_target_fd + 1).unwrap();
        let preserved_fd = preserved.as_raw_fd();
        let first = u32::try_from(first_target_fd).unwrap();
        let last = u32::try_from(last_target_fd).unwrap();

        match unsafe { fork_skip_atfork_handlers(KeepChildSignalsBlocked::Yes) } {
            RawFork::Child => unsafe {
                #[cfg(target_os = "linux")]
                let enumerated = ChildFdPlan::close_open_fds_from_proc(first, last);
                #[cfg(target_os = "macos")]
                let enumerated = ChildFdPlan::close_open_fds_from_dev(first, last);
                let targets_are_closed = targets
                    .iter()
                    .all(|target| libc::fcntl(target.as_raw_fd(), libc::F_GETFD) == -1);
                let lower_fd_is_open = libc::fcntl(file.as_raw_fd(), libc::F_GETFD) != -1;
                let higher_fd_is_open = libc::fcntl(preserved_fd, libc::F_GETFD) != -1;
                libc::_exit(
                    if enumerated && targets_are_closed && lower_fd_is_open && higher_fd_is_open {
                        0
                    } else {
                        1
                    },
                );
            },
            RawFork::Parent(pid) => crate::assert_child_exit!(pid, 0),
            RawFork::Error(error) => panic!("fork failed with errno {error}"),
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn descriptor_directory_walker_handles_its_own_fd_in_range() {
        match unsafe { fork_skip_atfork_handlers(KeepChildSignalsBlocked::Yes) } {
            RawFork::Child => unsafe {
                #[cfg(target_os = "linux")]
                let enumerated = ChildFdPlan::close_open_fds_from_proc(0, u32::MAX);
                #[cfg(target_os = "macos")]
                let enumerated = ChildFdPlan::close_open_fds_from_dev(0, u32::MAX);
                libc::_exit(if enumerated { 0 } else { 1 });
            },
            RawFork::Parent(pid) => crate::assert_child_exit!(pid, 0),
            RawFork::Error(error) => panic!("fork failed with errno {error}"),
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn proc_fd_walker_preserves_exec_fd_between_ranges() {
        let file = File::open("/dev/null").unwrap();
        let below = ChildFdPlan::duplicate_fd_at_least(file.as_raw_fd(), 64).unwrap();
        let exec_fd =
            ChildFdPlan::duplicate_fd_at_least(file.as_raw_fd(), below.as_raw_fd() + 1).unwrap();
        let above =
            ChildFdPlan::duplicate_fd_at_least(file.as_raw_fd(), exec_fd.as_raw_fd() + 1).unwrap();
        let first = u32::try_from(below.as_raw_fd()).unwrap();
        let exec_fd_number = u32::try_from(exec_fd.as_raw_fd()).unwrap();

        match unsafe { fork_skip_atfork_handlers(KeepChildSignalsBlocked::Yes) } {
            RawFork::Child => unsafe {
                let closed_below =
                    ChildFdPlan::close_open_fds_from_proc(first, exec_fd_number.saturating_sub(1));
                let closed_above = ChildFdPlan::close_open_fds_from_proc(
                    exec_fd_number.saturating_add(1),
                    u32::MAX,
                );
                let below_is_closed = libc::fcntl(below.as_raw_fd(), libc::F_GETFD) == -1;
                let exec_fd_is_open = libc::fcntl(exec_fd.as_raw_fd(), libc::F_GETFD) != -1;
                let above_is_closed = libc::fcntl(above.as_raw_fd(), libc::F_GETFD) == -1;
                libc::_exit(
                    if closed_below
                        && closed_above
                        && below_is_closed
                        && exec_fd_is_open
                        && above_is_closed
                    {
                        0
                    } else {
                        1
                    },
                );
            },
            RawFork::Parent(pid) => crate::assert_child_exit!(pid, 0),
            RawFork::Error(error) => panic!("fork failed with errno {error}"),
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn proc_fd_walker_reads_multiple_getdents_batches() {
        // Numeric procfs fd entries occupy at least 24 bytes each, so 192
        // descriptors cannot fit in the walker's 4 KiB buffer.
        let file = File::open("/dev/null").unwrap();
        let targets = (0..192)
            .map(|_| ChildFdPlan::duplicate_fd_at_least(file.as_raw_fd(), 64).unwrap())
            .collect::<Vec<_>>();
        let first = targets
            .iter()
            .map(AsRawFd::as_raw_fd)
            .min()
            .and_then(|fd| u32::try_from(fd).ok())
            .unwrap();
        let last = targets
            .iter()
            .map(AsRawFd::as_raw_fd)
            .max()
            .and_then(|fd| u32::try_from(fd).ok())
            .unwrap();

        match unsafe { fork_skip_atfork_handlers(KeepChildSignalsBlocked::Yes) } {
            RawFork::Child => unsafe {
                let batch_count = ChildFdPlan::enumerate_and_close_open_fds_from_proc(first, last);
                let targets_are_closed = targets
                    .iter()
                    .all(|target| libc::fcntl(target.as_raw_fd(), libc::F_GETFD) == -1);
                libc::_exit(
                    if batch_count.is_some_and(|count| count >= 2) && targets_are_closed {
                        0
                    } else {
                        1
                    },
                );
            },
            RawFork::Parent(pid) => crate::assert_child_exit!(pid, 0),
            RawFork::Error(error) => panic!("fork failed with errno {error}"),
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn child_fd_plan_keeps_required_fds_and_closes_others() {
        enum ExecFdPosition {
            Absent,
            BeforeStatus,
            AfterStatus,
        }

        for exec_position in [
            ExecFdPosition::Absent,
            ExecFdPosition::BeforeStatus,
            ExecFdPosition::AfterStatus,
        ] {
            let (pipe_read, pipe_write) = nix::unistd::pipe().unwrap();
            let stdio = [Stdio::Inherit, Stdio::Inherit, Stdio::Inherit];
            let (mut fd_plan, _status_read) = ChildFdPlan::new(
                &stdio[0],
                &stdio[1],
                &stdio[2],
                Some(pipe_write.as_fd()),
                std::iter::once(pipe_read),
            )
            .unwrap();
            // Put unwanted descriptors below, between, and above the two
            // candidates so every close range is exercised in either ordering.
            let file = File::open("/dev/null").unwrap();
            let below = fd_plan.duplicate_source(file.as_raw_fd()).unwrap();
            assert!(
                u32::try_from(fd_plan.status_write.as_raw_fd()).unwrap() >= fd_plan.first_to_close
            );
            let writer_above = |fd: RawFd| {
                let writer = nix::fcntl::fcntl(
                    fd_plan.status_write.as_raw_fd(),
                    nix::fcntl::FcntlArg::F_DUPFD_CLOEXEC(fd + 1),
                )
                .unwrap();
                // SAFETY: F_DUPFD_CLOEXEC returned a new descriptor owned by this test.
                unsafe { OwnedFd::from_raw_fd(writer) }
            };
            let lower_write = writer_above(below.as_raw_fd());
            let between =
                ChildFdPlan::duplicate_fd_at_least(file.as_raw_fd(), lower_write.as_raw_fd() + 1)
                    .unwrap();
            let higher_write = writer_above(between.as_raw_fd());
            let above =
                ChildFdPlan::duplicate_fd_at_least(file.as_raw_fd(), higher_write.as_raw_fd() + 1)
                    .unwrap();
            let (exec_file, status_write, unused_writer) = match exec_position {
                ExecFdPosition::Absent => (None, lower_write, Some(higher_write)),
                ExecFdPosition::BeforeStatus => (Some(lower_write), higher_write, None),
                ExecFdPosition::AfterStatus => (Some(higher_write), lower_write, None),
            };
            fd_plan.status_write = status_write;
            let exec_fd = exec_file.as_ref().map(AsRawFd::as_raw_fd);
            fd_plan.reserve_directory_fd().unwrap();
            let reservation_fd = fd_plan
                .directory_fd_reservation
                .as_ref()
                .unwrap()
                .as_raw_fd();
            assert!(reservation_fd >= fd_plan.source_fd_minimum);
            let temp_files = Vec::new();

            match unsafe { fork_skip_atfork_handlers(KeepChildSignalsBlocked::No) } {
                RawFork::Child => unsafe {
                    fd_plan.apply(exec_fd, &temp_files);
                    let required_are_open = libc::fcntl(3, libc::F_GETFD) != -1
                        && libc::fcntl(4, libc::F_GETFD) != -1
                        && exec_fd.is_none_or(|fd| libc::fcntl(fd, libc::F_GETFD) != -1)
                        && libc::fcntl(fd_plan.status_write.as_raw_fd(), libc::F_GETFD)
                            == libc::FD_CLOEXEC;
                    let unwanted_are_closed = [
                        below.as_raw_fd(),
                        between.as_raw_fd(),
                        above.as_raw_fd(),
                        reservation_fd,
                    ]
                    .iter()
                    .all(|fd| libc::fcntl(*fd, libc::F_GETFD) == -1);
                    let unused_candidate_is_closed = unused_writer
                        .as_ref()
                        .is_none_or(|fd| libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) == -1);
                    libc::_exit(
                        if required_are_open && unwanted_are_closed && unused_candidate_is_closed {
                            0
                        } else {
                            1
                        },
                    );
                },
                RawFork::Parent(pid) => crate::assert_child_exit!(pid, 0),
                RawFork::Error(error) => panic!("fork failed with errno {error}"),
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn child_fd_plan_reuses_safe_sources_without_losing_ownership() {
        let file = File::open("/dev/null").unwrap();
        let passed = ChildFdPlan::duplicate_fd_at_least(file.as_raw_fd(), 4).unwrap();
        let dependency = ChildFdPlan::duplicate_fd_at_least(file.as_raw_fd(), 4).unwrap();
        let passed_source = passed.as_raw_fd();
        let dependency_source = dependency.as_raw_fd();
        let last_source = passed_source.max(dependency_source);
        let dependency_count = usize::try_from(last_source - 3).unwrap();
        let mut dependencies = Vec::with_capacity(dependency_count);
        dependencies.push(dependency);
        for _ in 1..dependency_count {
            dependencies.push(ChildFdPlan::duplicate_fd_at_least(file.as_raw_fd(), 4).unwrap());
        }
        let stdio = [Stdio::Inherit, Stdio::Inherit, Stdio::Inherit];

        let (fd_plan, _status_read) = ChildFdPlan::new(
            &stdio[0],
            &stdio[1],
            &stdio[2],
            Some(passed.as_fd()),
            dependencies.into_iter(),
        )
        .unwrap();

        assert!(passed_source < fd_plan.source_fd_minimum);
        assert!(dependency_source < fd_plan.source_fd_minimum);
        assert!(matches!(
            &fd_plan.mappings[0].source,
            MaybeOwnedFd::Borrowed(fd) if fd.as_raw_fd() == passed_source
        ));
        assert!(matches!(
            &fd_plan.mappings[1].source,
            MaybeOwnedFd::Owned(fd) if fd.as_raw_fd() == dependency_source
        ));
        assert_ne!(unsafe { libc::fcntl(dependency_source, libc::F_GETFD) }, -1);
    }

    struct DropNotifier(RawFd);

    impl Drop for DropNotifier {
        fn drop(&mut self) {
            const DROPPED: &[u8] = b"d";
            unsafe {
                libc::write(
                    self.0,
                    DROPPED.as_ptr().cast::<libc::c_void>(),
                    DROPPED.len(),
                );
            }
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn fd_exec_uses_prepared_fallback_when_execveat_fails() {
        let fd = OwnedFd::from(File::open("/dev/null").unwrap());
        let fallback_path = linux::prepare_fallback_path().unwrap();
        assert_eq!(
            Err(std::io::ErrorKind::NotFound),
            std::fs::metadata(std::ffi::OsStr::from_bytes(fallback_path.to_bytes()))
                .map(|_| ())
                .map_err(|error| error.kind())
        );
        let prepared_spawn = PreparedSpawn::FdExec {
            fd,
            fallback_path: Some(fallback_path.clone()),
        };
        let mut argv = ExecVec::empty();
        argv.push(CString::new("spawn_worker_fallback_test").unwrap());
        argv.push(CString::new("").unwrap());
        argv.push(CString::new("__dummy_mirror_test").unwrap());
        argv.push(CString::new("symbol_name").unwrap());
        let envp = ExecVec::empty();
        let temp_files = Vec::new();
        let (fd_plan, _status_read) = empty_fd_plan();

        match unsafe { fork_skip_atfork_handlers(KeepChildSignalsBlocked::No) } {
            RawFork::Child => unsafe {
                libc::syscall(libc::SYS_umask, 0o777);
                prepared_spawn.exec(&mut argv, &envp, fd_plan.status_write.as_fd(), &temp_files)
            },
            RawFork::Parent(pid) => {
                crate::assert_child_exit!(pid, 0);
                assert_eq!(
                    Err(std::io::ErrorKind::NotFound),
                    std::fs::metadata(std::ffi::OsStr::from_bytes(fallback_path.to_bytes()))
                        .map(|_| ())
                        .map_err(|error| error.kind())
                );
            }
            RawFork::Error(error) => panic!("fork failed with errno {error}"),
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn fd_exec_does_not_materialize_fallback_when_execveat_succeeds() {
        let fallback_path = linux::prepare_fallback_path().unwrap();
        let fd = OwnedFd::from(linux::write_trampoline().unwrap().into_file());
        let prepared_spawn = PreparedSpawn::FdExec {
            fd,
            fallback_path: Some(fallback_path.clone()),
        };
        let mut argv = ExecVec::empty();
        argv.push(CString::new("spawn_worker_memfd_test").unwrap());
        argv.push(CString::new("").unwrap());
        argv.push(CString::new("__dummy_mirror_test").unwrap());
        argv.push(CString::new("symbol_name").unwrap());
        let envp = ExecVec::empty();
        let temp_files = Vec::new();
        let (fd_plan, _status_read) = empty_fd_plan();

        match unsafe { fork_skip_atfork_handlers(KeepChildSignalsBlocked::No) } {
            RawFork::Child => {
                prepared_spawn.exec(&mut argv, &envp, fd_plan.status_write.as_fd(), &temp_files)
            }
            RawFork::Parent(pid) => {
                crate::assert_child_exit!(pid, 0);
                assert_eq!(
                    Err(std::io::ErrorKind::NotFound),
                    std::fs::metadata(std::ffi::OsStr::from_bytes(fallback_path.to_bytes()))
                        .map(|_| ())
                        .map_err(|error| error.kind())
                );
            }
            RawFork::Error(error) => panic!("fork failed with errno {error}"),
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn fd_exec_fallback_does_not_replace_an_existing_file() {
        const ORIGINAL: &[u8] = b"not the trampoline";

        let mut existing = tempfile::NamedTempFile::new().unwrap();
        existing.write_all(ORIGINAL).unwrap();
        let fallback_path = CString::new(existing.path().as_os_str().as_bytes()).unwrap();
        let fd = OwnedFd::from(File::open("/dev/null").unwrap());
        let prepared_spawn = PreparedSpawn::FdExec {
            fd,
            fallback_path: Some(fallback_path),
        };
        let mut argv = ExecVec::empty();
        argv.push(CString::new("spawn_worker_fallback_collision_test").unwrap());
        argv.push(CString::new("").unwrap());
        let envp = ExecVec::empty();
        let temp_files = Vec::new();
        let (fd_plan, _status_read) = empty_fd_plan();

        match unsafe { fork_skip_atfork_handlers(KeepChildSignalsBlocked::No) } {
            RawFork::Child => {
                prepared_spawn.exec(&mut argv, &envp, fd_plan.status_write.as_fd(), &temp_files)
            }
            RawFork::Parent(pid) => {
                crate::assert_child_exit!(pid, 1);
                assert_eq!(ORIGINAL, std::fs::read(existing.path()).unwrap());
            }
            RawFork::Error(error) => panic!("fork failed with errno {error}"),
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn exec_failure_exits_without_unwinding() {
        let mut pipe_fds = [-1; 2];
        assert_eq!(0, unsafe { libc::pipe(pipe_fds.as_mut_ptr()) });
        // SAFETY: pipe returned two descriptors owned by this test.
        let pipe_read = unsafe { OwnedFd::from_raw_fd(pipe_fds[0]) };
        // SAFETY: pipe returned two descriptors owned by this test.
        let pipe_write = unsafe { OwnedFd::from_raw_fd(pipe_fds[1]) };
        let mut argv = ExecVec::empty();
        let envp = ExecVec::empty();
        let prepared_spawn =
            PreparedSpawn::Exec(CString::new("/spawn_worker/path/that/does/not/exist").unwrap());
        let temp_files = Vec::new();
        let (fd_plan, _status_read) = empty_fd_plan();

        match unsafe { fork_skip_atfork_handlers(KeepChildSignalsBlocked::No) } {
            RawFork::Child => {
                let _drop_notifier = DropNotifier(pipe_write.as_raw_fd());
                prepared_spawn.exec(&mut argv, &envp, fd_plan.status_write.as_fd(), &temp_files);
            }
            RawFork::Parent(pid) => {
                drop(pipe_write);
                crate::assert_child_exit!(pid, 1);
                let mut notification = [0_u8; 1];
                assert_eq!(0, unsafe {
                    libc::read(
                        pipe_read.as_raw_fd(),
                        notification.as_mut_ptr().cast::<libc::c_void>(),
                        notification.len(),
                    )
                });
            }
            RawFork::Error(error) => panic!("fork failed with errno {error}"),
        }
    }

    fn test_target() -> Target {
        Target::ManualTrampoline("__dummy_mirror_test".into(), "symbol_name".into())
    }

    fn empty_fd_plan() -> (ChildFdPlan<'static>, File) {
        ChildFdPlan::new(
            &Stdio::Inherit,
            &Stdio::Inherit,
            &Stdio::Inherit,
            None,
            std::iter::empty(),
        )
        .unwrap()
    }

    fn assert_child_reaped(pid: libc::pid_t) {
        assert_eq!(
            Err(nix::errno::Errno::ECHILD),
            nix::sys::wait::waitpid(
                Some(Pid::from_raw(pid)),
                Some(nix::sys::wait::WaitPidFlag::WNOHANG),
            ),
        );
    }
}
