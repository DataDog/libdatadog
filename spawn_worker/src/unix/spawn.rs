// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

use std::fs::File;
use std::{
    env,
    ffi::{self, CString, OsString},
    fs::Permissions,
    io::{Seek, Write},
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
            Target::Noop => return Ok(default_method),
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
    pub fn from_env<E: IntoIterator<Item = (ffi::OsString, ffi::OsString)>>(env: E) -> Self {
        Self {
            stdin: Stdio::Inherit,
            stdout: Stdio::Inherit,
            stderr: Stdio::Inherit,
            daemonize: false,
            target: Target::Noop,
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
    pub unsafe fn new() -> Self {
        Self::from_env(env::vars_os())
    }

    pub fn target<T: Into<Target>>(&mut self, target: T) -> &mut Self {
        self.target = target.into();
        self
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

    pub fn wait_spawn(&mut self) -> anyhow::Result<()> {
        let Child { pid } = self.spawn()?;
        Self::wait_pid(pid)
    }

    pub fn spawn(&mut self) -> anyhow::Result<Child> {
        let pid = self.do_spawn()?;

        Ok(Child { pid })
    }

    fn do_spawn(&self) -> anyhow::Result<Option<libc::pid_t>> {
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
            Target::Noop => return Ok(None),
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
        let fd_plan = ChildFdPlan::new(
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
                return Ok(Some(child_pid));
            }
            RawFork::Error(error) => {
                // temp_files is dropped here and unlinks its paths.
                anyhow::bail!(std::io::Error::from_raw_os_error(error));
            }
            RawFork::Child => {}
        }

        // We're in the (first) child. We can't drop memory
        let mut argv = ManuallyDrop::new(argv);
        let temp_files = ManuallyDrop::new(temp_files);
        let envp = ManuallyDrop::new(envp);

        if daemonize {
            // A detached worker must not inherit the embedding runtime's signal
            // policy. Unlike execve, this also resets dispositions set to SIG_IGN.
            if unsafe { reset_signal_dispositions() }.is_err() {
                unsafe {
                    child_fail(
                        temp_files.as_slice(),
                        b"spawn_worker: resetting signal dispositions failed\n",
                    );
                }
            }
            // Start a new session and process group and drop the inherited
            // controlling terminal before the final fork.
            if unsafe { libc::setsid() } == -1 {
                unsafe {
                    child_fail(temp_files.as_slice(), b"spawn_worker: setsid failed\n");
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
                    child_fail(temp_files.as_slice(), b"spawn_worker: daemon fork failed\n");
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
                        temp_files.as_slice(),
                        b"spawn_worker: clearing signal mask failed\n",
                    );
                }
            }
        }

        // Replace the child with the prepared executable.
        prepared_spawn.exec(&mut argv, &envp, temp_files.as_slice());
    }

    fn wait_pid(pid: Option<libc::pid_t>) -> anyhow::Result<()> {
        let pid = match pid {
            Some(pid) => Pid::from_raw(pid),
            None => return Ok(()),
        };

        nix::sys::wait::waitpid(Some(pid), None)?;
        Ok(())
    }
}

pub struct Child {
    pub pid: Option<libc::pid_t>,
}

impl Child {
    pub fn wait(self) -> anyhow::Result<WaitStatus> {
        // Command::spawn(&mut self);
        let pid = match self.pid {
            Some(pid) => Pid::from_raw(pid),
            None => return Ok(WaitStatus::Exited(Pid::from_raw(0), 0)),
        };

        Ok(nix::sys::wait::waitpid(Some(pid), None)?)
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
/// the mappings are installed, the plan preserves a required exec fd and tries
/// to close higher fds using Linux close_range or a precomputed scan bound.
pub(crate) struct ChildFdPlan<'fd> {
    stdio: [ChildStdio; 3],
    mappings: Vec<FdMapping<'fd>>,
    source_fd_minimum: RawFd,
    first_to_close: u32,
    fallback_last: u32,
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
    /// Build the complete descriptor plan. Must be called before the fork.
    pub(crate) fn new<I>(
        stdin: &Stdio,
        stdout: &Stdio,
        stderr: &Stdio,
        passed_fd: Option<BorrowedFd<'fd>>,
        dependency_fds: I,
    ) -> anyhow::Result<Self>
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

        let mut plan = Self {
            stdio: [
                stdin.as_child_stdio()?,
                stdout.as_child_stdio()?,
                stderr.as_child_stdio()?,
            ],
            mappings: Vec::with_capacity(usize::from(has_passed_fd) + dependency_count),
            source_fd_minimum,
            first_to_close: u32::try_from(source_fd_minimum)?,
            fallback_last: Self::fd_limit()?,
        };

        if let Some(fd) = passed_fd {
            plan.add_borrowed_mapping(fd, 3)?;
        }
        for (index, fd) in dependency_fds.enumerate() {
            plan.add_owned_mapping(fd, dependency_destination + RawFd::try_from(index)?)?;
        }

        Ok(plan)
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

    /// Apply the plan after the fork. Must be called only in the child.
    pub(crate) unsafe fn apply(&self, exec_fd: Option<RawFd>, temp_files: &[CString]) {
        for (child_stdio, destination) in self.stdio.iter().zip(0..=libc::STDERR_FILENO) {
            if let Some(source) = child_stdio.as_fd() {
                if unsafe { libc::dup2(source, destination) } == -1 {
                    unsafe {
                        child_fail(temp_files, b"spawn_worker: dup2 failed\n");
                    }
                }
            }
        }

        for mapping in &self.mappings {
            if unsafe { libc::dup2(mapping.source.as_raw_fd(), mapping.destination) } == -1 {
                unsafe {
                    child_fail(temp_files, b"spawn_worker: dup2 failed\n");
                }
            }
        }

        if let Some(exec_fd) = exec_fd {
            let exec_fd = exec_fd as u32;
            unsafe {
                Self::close_fd_range(
                    self.first_to_close,
                    exec_fd.saturating_sub(1),
                    self.fallback_last,
                );
                Self::close_fd_range(exec_fd.saturating_add(1), u32::MAX, self.fallback_last);
            }
        } else {
            unsafe {
                Self::close_fd_range(self.first_to_close, u32::MAX, self.fallback_last);
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

    fn fd_limit() -> std::io::Result<u32> {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == -1 {
            return Err(std::io::Error::last_os_error());
        }

        // TODO: A descriptor opened before the hard limit is lowered can remain
        // above rlim_max and escape the fallback scan when close_range is unavailable.
        let descriptor_count = if limit.rlim_max == libc::RLIM_INFINITY {
            Self::system_fd_limit()?
        } else {
            limit.rlim_max
        };
        let highest_raw_fd = libc::c_int::MAX as libc::rlim_t;
        Ok(descriptor_count.saturating_sub(1).min(highest_raw_fd) as u32)
    }

    #[cfg(target_os = "linux")]
    fn system_fd_limit() -> std::io::Result<libc::rlim_t> {
        let nr_open = std::fs::read_to_string("/proc/sys/fs/nr_open")?;
        nr_open
            .trim()
            .parse::<libc::rlim_t>()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    }

    #[cfg(target_os = "macos")]
    fn system_fd_limit() -> std::io::Result<libc::rlim_t> {
        let mut maximum: libc::c_int = 0;
        let mut size = std::mem::size_of_val(&maximum);
        let result = unsafe {
            libc::sysctlbyname(
                c"kern.maxfilesperproc".as_ptr(),
                (&mut maximum as *mut libc::c_int).cast::<libc::c_void>(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if result == -1 {
            return Err(std::io::Error::last_os_error());
        }
        libc::rlim_t::try_from(maximum)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn system_fd_limit() -> std::io::Result<libc::rlim_t> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "cannot determine the system file descriptor limit",
        ))
    }

    // Use separate last and fallback_last because using last=u32::MAX is
    // better than relying on RLIMIT_NOFILE, but using a very high number
    // for close_open_fds would be prohibitively slow.
    #[cfg(target_os = "linux")]
    unsafe fn close_fd_range(first: u32, last: u32, fallback_last: u32) {
        if first > last {
            return;
        }
        if unsafe { libc::syscall(libc::SYS_close_range, first, last, 0) } == 0 {
            return;
        }
        unsafe {
            Self::close_open_fds(first, last.min(fallback_last));
        }
    }

    #[cfg(not(target_os = "linux"))]
    unsafe fn close_fd_range(first: u32, last: u32, fallback_last: u32) {
        unsafe {
            Self::close_open_fds(first, last.min(fallback_last));
        }
    }

    unsafe fn close_open_fds(first: u32, last: u32) {
        const POLL_BATCH_SIZE: usize = 256;

        if first > last {
            return;
        }

        let mut poll_fds = [libc::pollfd {
            fd: -1,
            events: 0,
            revents: 0,
        }; POLL_BATCH_SIZE];
        let mut next_fd = first;
        loop {
            let mut count = 0;
            while count < POLL_BATCH_SIZE && next_fd <= last {
                // SAFETY: count is checked against POLL_BATCH_SIZE above.
                let poll_fd = unsafe { poll_fds.get_unchecked_mut(count) };
                poll_fd.fd = next_fd as RawFd;
                poll_fd.revents = 0;
                count += 1;
                next_fd += 1;
            }

            let poll_result =
                unsafe { Self::poll_open_fds(poll_fds.as_mut_ptr(), count as libc::nfds_t) };
            for index in 0..count {
                // SAFETY: index is bounded by count, which is at most POLL_BATCH_SIZE.
                let poll_fd = unsafe { poll_fds.get_unchecked(index) };
                if poll_result == -1 || poll_fd.revents & libc::POLLNVAL == 0 {
                    unsafe {
                        Self::close_fd(poll_fd.fd);
                    }
                }
            }

            if next_fd > last {
                return;
            }
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

    #[cfg(target_os = "linux")]
    unsafe fn poll_open_fds(poll_fds: *mut libc::pollfd, count: libc::nfds_t) -> libc::c_int {
        // libc poll is a cancellation point on musl. Use the raw ppoll syscall so
        // a raw-fork child never enters libc's unrepaired cancellation machinery.
        let timeout = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe {
            libc::syscall(
                libc::SYS_ppoll,
                poll_fds,
                count,
                &timeout,
                std::ptr::null::<libc::sigset_t>(),
                std::mem::size_of::<u64>(),
            ) as libc::c_int
        }
    }

    #[cfg(not(target_os = "linux"))]
    unsafe fn poll_open_fds(poll_fds: *mut libc::pollfd, count: libc::nfds_t) -> libc::c_int {
        unsafe { libc::poll(poll_fds, count, 0) }
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
    fn exec(&self, argv: &mut ExecVec, envp: &ExecVec, temp_files: &[CString]) -> ! {
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
            child_fail(temp_files, b"spawn_worker: exec failed\n")
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

unsafe fn child_fail(temp_files: &[CString], message: &'static [u8]) -> ! {
    for temp_file in temp_files {
        unsafe {
            // unlink() is async-signal-safe
            libc::unlink(temp_file.as_ptr());
        }
    }
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
    fn child_fd_plan_keeps_required_fds_and_closes_others() {
        let mut pipe_fds = [-1; 2];
        assert_eq!(0, unsafe { libc::pipe(pipe_fds.as_mut_ptr()) });
        // SAFETY: pipe returned two descriptors owned by this test.
        let pipe_read = unsafe { OwnedFd::from_raw_fd(pipe_fds[0]) };
        // SAFETY: pipe returned two descriptors owned by this test.
        let pipe_write = unsafe { OwnedFd::from_raw_fd(pipe_fds[1]) };
        let stdio = [Stdio::Inherit, Stdio::Inherit, Stdio::Inherit];
        let fd_plan = ChildFdPlan::new(
            &stdio[0],
            &stdio[1],
            &stdio[2],
            Some(pipe_write.as_fd()),
            std::iter::once(pipe_read),
        )
        .unwrap();
        let unwanted_file = File::open("/dev/null").unwrap();
        let unwanted = fd_plan.duplicate_source(unwanted_file.as_raw_fd()).unwrap();
        let temp_files = Vec::new();

        match unsafe { fork_skip_atfork_handlers(KeepChildSignalsBlocked::No) } {
            RawFork::Child => unsafe {
                fd_plan.apply(None, &temp_files);
                let required_are_open =
                    libc::fcntl(3, libc::F_GETFD) != -1 && libc::fcntl(4, libc::F_GETFD) != -1;
                let unwanted_is_closed = libc::fcntl(unwanted.as_raw_fd(), libc::F_GETFD) == -1;
                libc::_exit(if required_are_open && unwanted_is_closed {
                    0
                } else {
                    1
                });
            },
            RawFork::Parent(pid) => crate::assert_child_exit!(pid, 0),
            RawFork::Error(error) => panic!("fork failed with errno {error}"),
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

        let fd_plan = ChildFdPlan::new(
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

        match unsafe { fork_skip_atfork_handlers(KeepChildSignalsBlocked::No) } {
            RawFork::Child => unsafe {
                libc::syscall(libc::SYS_umask, 0o777);
                prepared_spawn.exec(&mut argv, &envp, &temp_files)
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

        match unsafe { fork_skip_atfork_handlers(KeepChildSignalsBlocked::No) } {
            RawFork::Child => prepared_spawn.exec(&mut argv, &envp, &temp_files),
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

        match unsafe { fork_skip_atfork_handlers(KeepChildSignalsBlocked::No) } {
            RawFork::Child => prepared_spawn.exec(&mut argv, &envp, &temp_files),
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

        match unsafe { fork_skip_atfork_handlers(KeepChildSignalsBlocked::No) } {
            RawFork::Child => {
                let _drop_notifier = DropNotifier(pipe_write.as_raw_fd());
                prepared_spawn.exec(&mut argv, &envp, &temp_files);
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
}
