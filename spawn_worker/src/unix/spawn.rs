// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
#![cfg(unix)]

#[cfg(target_os = "linux")]
mod linux {
    use std::io::{Seek, Write};

    use crate::TRAMPOLINE_BIN;

    pub(crate) fn write_trampoline() -> anyhow::Result<memfd::Memfd> {
        let opts = memfd::MemfdOptions::default();
        let mfd = opts.create("spawn_worker_trampoline")?;

        mfd.as_file().set_len(TRAMPOLINE_BIN.len() as u64)?;
        mfd.as_file().write_all(TRAMPOLINE_BIN)?;
        mfd.as_file().rewind()?;

        Ok(mfd)
    }
}
use std::fs::File;

use std::path::PathBuf;
use std::{
    env,
    ffi::{self, CString, OsString},
    fs::Permissions,
    io::{Seek, Write},
    os::unix::prelude::{AsRawFd, OsStringExt, PermissionsExt},
    ptr,
};

use io_lifetimes::OwnedFd;

use nix::{sys::wait::WaitStatus, unistd::Pid};

use crate::fork::{fork, Fork};
use nix::libc;

struct ExecVec {
    items: Vec<CString>,
    // Always NULL ptr terminated
    ptrs: Vec<*const libc::c_char>,
}

impl ExecVec {
    fn as_ptr(&self) -> *const *const libc::c_char {
        self.ptrs.as_ptr()
    }

    fn empty() -> Self {
        Self {
            items: vec![],
            ptrs: vec![std::ptr::null()],
        }
    }

    pub fn push(&mut self, item: CString) {
        let l = self.ptrs.len();
        // replace previous trailing null with ptr to the item
        self.ptrs[l - 1] = item.as_ptr();
        self.ptrs.push(ptr::null());
        self.items.push(item);
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

#[derive(Clone)]
pub enum SpawnMethod {
    #[cfg(target_os = "linux")]
    FdExec,
    LdPreload,
    Exec,
}

pub enum Target {
    Entrypoint(crate::Entrypoint),
    Manual(CString, CString),
    Noop,
}

impl Target {
    /// Automatically detect which spawn method should be used
    pub fn detect_spawn_method(&self) -> std::io::Result<SpawnMethod> {
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
            Target::Manual(p, _) => p
                .to_str()
                .map(PathBuf::from)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)),
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

impl From<File> for Stdio {
    fn from(val: File) -> Self {
        Stdio::Fd(val.into())
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

    pub fn spawn(&mut self) -> anyhow::Result<Child> {
        let pid = self.do_spawn()?;

        Ok(Child { pid })
    }

    fn do_spawn(&self) -> anyhow::Result<Option<libc::pid_t>> {
        let mut argv = ExecVec::empty();
        // set argv[0] and process name shown eg in `ps`
        let process_name = CString::new(self.process_name.as_deref().unwrap_or("spawned_worker"))?;
        argv.push(process_name);

        match &self.target {
            Target::Entrypoint(entrypoint) => {
                let path = match unsafe {
                    crate::get_dl_path_raw(entrypoint.ptr as *const libc::c_void)
                } {
                    (Some(path), _) => path,
                    _ => return Err(anyhow::format_err!("can'taaa read symbol pointer data")),
                };

                argv.push(path);
                argv.push(entrypoint.symbol_name.clone());
            }
            Target::Manual(path, symbol_name) => {
                argv.push(path.clone());
                argv.push(symbol_name.clone());
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

        // setup arbitrary fd passing
        let _shorter_lived_fd = if let Some(src_fd) = &self.fd_to_pass {
            // we're stripping the close on exec flag from the FD
            // to ensure we will not modify original fd, whose expected lifetime is unknown
            // we should clone the FD that needs passing to the subprocess, keeping its lifetime
            // as short as possible

            // rationale: some FDs are more important than others
            //      e.g. listening socket fd must not be accidentally leaked to a subprocess
            //      this would cause hard to debug bugs where random process could block the address
            //  TODO: this method is not perfect, ideally we should create an anonymous socket pair
            //        then send any FDs through that socket pair. Ensuring no random spawned processes could leak
            let fd = src_fd.try_clone()?;
            envp.push(CString::new(format!(
                "{}={}",
                crate::ENV_PASS_FD_KEY,
                fd.as_raw_fd()
            ))?);
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

        // setup final spawn

        let spawn_method = match &self.spawn_method {
            Some(m) => m.clone(),
            None => self.target.detect_spawn_method()?,
        };

        // build and allocate final exec fn and its dependencies
        let spawn: Box<dyn Fn()> = match spawn_method {
            #[cfg(target_os = "linux")]
            SpawnMethod::FdExec => {
                let fd = linux::write_trampoline()?;
                Box::new(move || {
                    // not using nix crate here, as it would allocate args after fork, which will lead to crashes on systems
                    // where allocator is not fork+thread safe
                    unsafe { libc::fexecve(fd.as_raw_fd(), argv.as_ptr(), envp.as_ptr()) };
                    // if we're here then exec has failed
                    panic!("{}", std::io::Error::last_os_error());
                })
            }
            SpawnMethod::LdPreload => {
                let lib_path = write_to_tmp_file(crate::LD_PRELOAD_TRAMPOLINE_LIB)?
                    .into_temp_path()
                    .keep()?;
                let env_prefix = "LD_PRELOAD=";

                let mut ld_env =
                    OsString::with_capacity(env_prefix.len() + lib_path.as_os_str().len() + 1);

                ld_env.push(env_prefix);
                ld_env.push(lib_path);
                envp.push(CString::new(ld_env.into_vec())?);

                let path = CString::new(env::current_exe()?.to_str().ok_or_else(|| {
                    anyhow::format_err!("can't convert current executable file to correct path")
                })?)?;

                Box::new(move || unsafe {
                    libc::execve(path.as_ptr(), argv.as_ptr(), envp.as_ptr());
                    // if we're here then exec has failed
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

                Box::new(move || {
                    // not using nix crate here, to avoid allocations post fork
                    unsafe { libc::execve(path.as_ptr(), argv.as_ptr(), envp.as_ptr()) };
                    // if we're here then exec has failed
                    panic!("{}", std::io::Error::last_os_error());
                })
            }
        };
        let stdin = self.stdin.as_child_stdio()?;
        let stdout = self.stdout.as_child_stdio()?;
        let stderr = self.stderr.as_child_stdio()?;

        // no allocations in the child process should happen by this point for maximum safety
        if let Fork::Parent(child_pid) = unsafe { fork()? } {
            return Ok(Some(child_pid));
        }

        if self.daemonize {
            if let Fork::Parent(_) = unsafe { fork()? } {
                std::process::exit(0);
            }
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
