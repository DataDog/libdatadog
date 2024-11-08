// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

#[cfg(target_os = "linux")]
mod linux {
    use std::io::{Seek, Write};

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

    use crate::TRAMPOLINE_BIN;
    pub(crate) fn write_trampoline() -> anyhow::Result<memfd::Memfd> {
        write_memfd("spawn_worker_trampoline", TRAMPOLINE_BIN)
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
    }
}

fn write_to_tmp_file(data: &[u8]) -> anyhow::Result<tempfile::NamedTempFile> {
    let tmp_file = tempfile::NamedTempFile::new()?;
    let mut file = tmp_file.as_file();
    file.set_len(data.len() as u64)?;
    file.write_all(data)?;
    file.rewind()?;

    std::fs::set_permissions(tmp_file.path(), Permissions::from_mode(0o700))?;

    Ok(tmp_file)
}

use std::fs::File;

#[cfg(target_os = "linux")]
use std::ffi::CStr;
use std::io;
use std::ops::RangeInclusive;
use std::{
    env,
    ffi::{self, CString, OsString},
    fs::Permissions,
    io::{Seek, Write},
    os::unix::prelude::{AsRawFd, OsStringExt, PermissionsExt},
};

use io_lifetimes::OwnedFd;

use nix::{sys::wait::WaitStatus, unistd::Pid};

use crate::fork::{fork, Fork};
use nix::libc;

#[derive(Clone)]
pub enum SpawnMethod {
    #[cfg(target_os = "linux")]
    FdExec,
    #[cfg(not(target_os = "macos"))]
    LdPreload,
    Exec,
}

use crate::unix::spawn::helper::ExecVec;
use crate::{LibDependency, Target};

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
            Target::Entrypoint(e) => e.get_fs_path().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "can't find the entrypoint's target path",
                )
            }),
            Target::ManualTrampoline(p, _) => Ok(std::path::PathBuf::from(p)),
            Target::Noop => return Ok(default_method),
        }?;
        let target_filename = target_path.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                "can't extract actual target filename",
            )
        })?;

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

enum ChildStdio {
    Inherit,
    Owned(OwnedFd),
    Ref(libc::pid_t),
}

impl ChildStdio {
    fn as_fd(&self) -> Option<libc::pid_t> {
        match self {
            ChildStdio::Inherit => None,
            ChildStdio::Owned(fd) => Some(fd.as_raw_fd()),
            ChildStdio::Ref(fd) => Some(*fd),
        }
    }
}

pub enum Stdio {
    Inherit,
    Fd(OwnedFd),
    Null,
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

    fn wait_pid(pid: Option<libc::pid_t>) -> anyhow::Result<()> {
        let pid = match pid {
            Some(pid) => Pid::from_raw(pid),
            None => return Ok(()),
        };

        nix::sys::wait::waitpid(Some(pid), None)?;
        Ok(())
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
        let mut argv = ExecVec::empty();
        // set argv[0] and process name shown eg in `ps`
        let process_name = CString::new(self.process_name.as_deref().unwrap_or("spawned_worker"))?;
        argv.push(process_name);
        argv.push(CString::new("")?);

        let entrypoint_symbol_name = match &self.target {
            Target::Entrypoint(entrypoint) => {
                let path = match unsafe {
                    crate::get_dl_path_raw(entrypoint.ptr as *const libc::c_void)
                } {
                    (Some(path), _) => path,
                    _ => return Err(anyhow::format_err!("can't read symbol pointer data")),
                };

                argv.push(path);
                entrypoint.symbol_name.clone()
            }
            Target::ManualTrampoline(path, symbol_name) => {
                argv.push(CString::new(path.as_str())?);
                CString::new(symbol_name.as_str())?
            }
            Target::Noop => return Ok(None),
        };

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

        let fd_to_pass = if let Some(src_fd) = &self.fd_to_pass {
            // FD to pass is always 4
            envp.push(CString::new(format!("{}={}", crate::ENV_PASS_FD_KEY, 3))?);

            let fd = src_fd.try_clone()?;

            // Strip any close on exec flag on this fd
            let flags = nix::fcntl::fcntl(fd.as_raw_fd(), nix::fcntl::F_GETFD)?;
            unsafe {
                libc::fcntl(
                    fd.as_raw_fd(),
                    libc::F_SETFD,
                    flags & !nix::libc::FD_CLOEXEC,
                )
            };
            Some(fd) // keep the temporary fd in scope for the duration of this method
        } else {
            None
        };
        // make sure the fd_to_pass is not dropped until the end of the function
        let fd_to_pass = fd_to_pass.as_ref();

        // setup final spawn

        #[allow(unused_mut)]
        let mut spawn_method = match &self.spawn_method {
            Some(m) => m.clone(),
            None => self.target.detect_spawn_method()?,
        };

        let mut temp_files = vec![];
        #[cfg(target_os = "linux")]
        let mut temp_memfds = vec![];
        for dep in &self.shared_lib_dependencies {
            match dep {
                LibDependency::Path(path) => {
                    argv.push(CString::new(path.to_string_lossy().to_string())?)
                }
                LibDependency::Binary(bin) => {
                    let mut tempfile = || -> anyhow::Result<()> {
                        let path = CString::new(
                            write_to_tmp_file(bin)?
                                .into_temp_path()
                                .keep()? // ensure the file is not auto cleaned in parent process
                                .as_os_str()
                                .to_str()
                                .ok_or_else(|| {
                                    anyhow::format_err!("can't convert tmp file path")
                                })?,
                        )?;
                        temp_files.push(path.clone());
                        argv.push(CString::new("-")?);
                        argv.push(path);
                        Ok(())
                    };
                    #[cfg(target_os = "linux")]
                    if matches!(spawn_method, SpawnMethod::FdExec) {
                        if let Ok(memfd) = linux::write_memfd("trampoline_dependencies.so", bin) {
                            let basefds = if fd_to_pass.is_some() { 4 } else { 3 };
                            argv.push(CString::new(format!(
                                "/proc/self/fd/{}",
                                temp_memfds.len() + basefds
                            ))?);
                            temp_memfds.push(memfd);
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

        // build and allocate final exec fn and its dependencies
        #[cfg(target_os = "linux")]
        let mut skip_close_fd = 0;
        #[cfg(not(target_os = "linux"))]
        let skip_close_fd = 0;

        let mut spawn: Box<dyn FnMut()> = match spawn_method {
            #[cfg(target_os = "linux")]
            SpawnMethod::FdExec => {
                let fd = linux::write_trampoline()?;
                skip_close_fd = fd.as_raw_fd();
                Box::new(move || unsafe {
                    // not using nix crate here, as it would allocate args after fork, which will
                    // lead to crashes on systems where allocator is not
                    // fork+thread safe
                    libc::fexecve(fd.as_raw_fd(), argv.as_ptr(), envp.as_ptr());

                    // if we're here then exec has failed
                    let fexecve_error = std::io::Error::last_os_error();

                    let mut temp_path = [0u8; 256];
                    let tmpdir = libc::getenv("TMPDIR".as_ptr() as *const libc::c_char)
                        as *const libc::c_char;
                    let tmpdir = if tmpdir.is_null() {
                        b"/tmp"
                    } else {
                        CStr::from_ptr(tmpdir).to_bytes()
                    };
                    if tmpdir.len() < 220 {
                        temp_path[..tmpdir.len()].copy_from_slice(tmpdir);
                        let mut off = tmpdir.len();
                        let spawn_prefix = b"/dd-ipc-spawn_";
                        temp_path[off..off + spawn_prefix.len()].copy_from_slice(spawn_prefix);
                        off += spawn_prefix.len();
                        for _ in 0..8 {
                            temp_path[off] = fastrand::alphanumeric() as u8;
                            off += 1;
                        }

                        let path = Vec::from_raw_parts(temp_path.as_mut_ptr(), off, off);
                        let path = CString::from_vec_with_nul_unchecked(path);
                        let path_ptr = path.as_ptr();
                        let tmpfd = libc::open(
                            path_ptr,
                            libc::O_CREAT | libc::O_RDWR,
                            libc::S_IRWXU as libc::c_uint,
                        );
                        if tmpfd < 0 {
                            // We'll leak it, executing Drop of path is forbidden.
                            std::mem::forget(path);
                        } else {
                            libc::sendfile(
                                tmpfd,
                                fd.as_raw_fd(),
                                std::ptr::null_mut(),
                                crate::TRAMPOLINE_BIN.len(),
                            );
                            libc::close(tmpfd);
                            argv.set(1, path);

                            libc::execve(path_ptr, argv.as_ptr(), envp.as_ptr());

                            libc::unlink(temp_path.as_ptr() as *const libc::c_char);
                        }
                    }

                    panic!("Failed lauching via fexecve(): {fexecve_error}");
                })
            }
            #[cfg(not(target_os = "macos"))]
            SpawnMethod::LdPreload => {
                let lib_path = write_to_tmp_file(crate::LD_PRELOAD_TRAMPOLINE_LIB)?
                    .into_temp_path()
                    .keep()?;
                let env_prefix = "LD_PRELOAD=";

                temp_files.push(CString::new(lib_path.to_str().unwrap())?);

                let mut ld_env =
                    OsString::with_capacity(env_prefix.len() + lib_path.as_os_str().len() + 1);

                ld_env.push(env_prefix);
                ld_env.push(lib_path);
                envp.push(CString::new(ld_env.into_vec())?);

                let path = CString::new(env::current_exe()?.to_str().ok_or_else(|| {
                    anyhow::format_err!("can't convert current executable file to correct path")
                })?)?;

                argv.set(1, path.clone());

                let ref_temp_files = &temp_files;
                Box::new(move || unsafe {
                    libc::execve(path.as_ptr(), argv.as_ptr(), envp.as_ptr());
                    // if we're here then exec has failed
                    for temp_file in ref_temp_files {
                        libc::unlink(temp_file.as_ptr());
                    }
                    panic!("{}", std::io::Error::last_os_error());
                })
            }
            SpawnMethod::Exec => {
                let path = CString::new(
                    write_to_tmp_file(crate::TRAMPOLINE_BIN)?
                        .into_temp_path()
                        .keep()? // ensure the file is not auto cleaned in parent process
                        .as_os_str()
                        .to_str()
                        .ok_or_else(|| anyhow::format_err!("can't convert tmp file path"))?,
                )?;

                temp_files.push(path.clone());
                argv.set(1, path.clone());

                let ref_temp_files = &temp_files;
                Box::new(move || unsafe {
                    // not using nix crate here, to avoid allocations post fork
                    libc::execve(path.as_ptr(), argv.as_ptr(), envp.as_ptr());
                    // if we're here then exec has failed
                    for temp_file in ref_temp_files {
                        libc::unlink(temp_file.as_ptr());
                    }
                    panic!("{}", std::io::Error::last_os_error());
                })
            }
        };
        let stdin = self.stdin.as_child_stdio()?;
        let stdout = self.stdout.as_child_stdio()?;
        let stderr = self.stderr.as_child_stdio()?;

        let ref_temp_files = &temp_files;
        let do_fork = || unsafe {
            match fork() {
                Ok(fork) => Ok(fork),
                Err(e) => {
                    for temp_file in ref_temp_files {
                        libc::unlink(temp_file.as_ptr());
                    }
                    Err(e)
                }
            }
        };

        // no allocations in the child process should happen by this point for maximum safety
        if let Fork::Parent(child_pid) = do_fork()? {
            return Ok(Some(child_pid));
        }

        if let Some(fd) = stdin.as_fd() {
            unsafe { libc::dup2(fd, libc::STDIN_FILENO) };
        }

        if let Some(fd) = stdout.as_fd() {
            unsafe { libc::dup2(fd, libc::STDOUT_FILENO) };
        }

        if let Some(fd) = stderr.as_fd() {
            unsafe { libc::dup2(fd, libc::STDERR_FILENO) };
        }

        #[allow(unused_mut)]
        let mut close_range = if let Some(fd) = fd_to_pass {
            unsafe { libc::dup2(fd.as_raw_fd(), 3) };
            4..=i32::MAX
        } else {
            3..=i32::MAX
        };

        #[cfg(target_os = "linux")]
        {
            let mut fdnum = *close_range.start();
            for fd in temp_memfds {
                unsafe { libc::dup2(fd.as_raw_fd(), fdnum) };
                fdnum += 1;
            }
            close_range = fdnum..=i32::MAX;
        }

        if let Err(_e) = close_fd_range(close_range, skip_close_fd) {
            // What do we do here?
            // /proc might not be mounted?
        }

        if self.daemonize {
            if let Fork::Parent(_) = do_fork()? {
                // musl will try to "correct" offsets in an atexit handler (lseek a FILE* to the
                // "true" position) Ensure all fds are closed so that musl cannot
                // have side-effects
                for i in 0..4 {
                    unsafe {
                        libc::close(i);
                    }
                }
                unsafe {
                    libc::close(skip_close_fd);
                    libc::_exit(0);
                }
            }
        }

        spawn();
        std::process::exit(1);
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

#[cfg(target_os = "macos")]
const SELF_FD_DIR: &str = "/dev/fd";

#[cfg(target_os = "linux")]
const SELF_FD_DIR: &str = "/proc/self/fd";

fn list_open_fds() -> io::Result<impl Iterator<Item = i32>> {
    let dir = nix::dir::Dir::open(
        SELF_FD_DIR,
        nix::fcntl::OFlag::O_DIRECTORY | nix::fcntl::OFlag::O_RDONLY,
        nix::sys::stat::Mode::empty(),
    )?;
    let dir_fd = dir.as_raw_fd();
    Ok(dir.into_iter().filter_map(move |fd_path| {
        let fd_path = match fd_path {
            Err(_) => return None,
            Ok(p) => p,
        };
        if fd_path.file_name().to_bytes() == b"." || fd_path.file_name().to_bytes() == b".." {
            return None;
        }
        let fd: i32 = std::str::from_utf8(fd_path.file_name().to_bytes())
            .unwrap()
            .parse()
            .unwrap();
        if fd == dir_fd {
            return None;
        }
        Some(fd)
    }))
}

// https://man7.org/linux/man-pages/man2/close_range.2.html is too recent sadly :_()
fn close_fd_range(range: RangeInclusive<i32>, skip_close_fd: i32) -> std::io::Result<()> {
    for fd in list_open_fds()? {
        if fd != skip_close_fd && range.contains(&fd) {
            nix::unistd::close(fd)?;
        }
    }
    Ok(())
}
