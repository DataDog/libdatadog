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
use nix::libc;

#[derive(Default)]
struct ExecVec {
    data: Vec<CString>,
    ptrs: Vec<*const libc::c_char>,
}

struct SealedExecVec {
    _data: Vec<CString>,
    ptrs: Vec<*const libc::c_char>,
}

impl SealedExecVec {
    fn as_ptr(&self) -> *const *const i8 {
        self.ptrs.as_ptr()
    }
}

impl ExecVec {
    fn push(&mut self, item: CString) {
        self.ptrs.push(item.as_ptr());
        self.data.push(item);
    }

    fn seal(mut self) -> SealedExecVec {
        self.ptrs.push(ptr::null());
        SealedExecVec {
            _data: self.data,
            ptrs: self.ptrs,
        }
    }
}

fn write_trampoline() -> anyhow::Result<tempfile::NamedTempFile> {
    let tmp_file = tempfile::NamedTempFile::new()?;
    let mut file = tmp_file.as_file();
    file.set_len(TRAMPOLINE_BIN.len() as u64)?;
    file.write_all(TRAMPOLINE_BIN)?;
    file.rewind()?;

    std::fs::set_permissions(tmp_file.path(), Permissions::from_mode(0o700))?;

    Ok(tmp_file)
}

pub enum SpawnMethod {
    #[cfg(target_os = "linux")]
    FdExec,
    Exec,
}

impl Default for SpawnMethod {
    #[cfg(target_os = "linux")]
    fn default() -> Self {
        Self::FdExec
    }

    #[cfg(not(target_os = "linux"))]
    fn default() -> Self {
        Self::Exec
    }
}

pub enum Target {
    Trampoline(extern "C" fn()),
    ManualTrampoline(CString, CString),
    Fork(fn()),
    Noop,
}

pub struct SpawnCfg {
    stdin: Option<OwnedFd>,
    stderr: Option<OwnedFd>,
    stdout: Option<OwnedFd>,
    spawn_method: SpawnMethod,
    target: Target,
    inherit_env: bool,
}

impl SpawnCfg {
    pub fn new() -> Self {
        Self {
            stdin: None,
            stdout: None,
            stderr: None,
            target: Target::Noop,
            inherit_env: true,
            spawn_method: Default::default(),
        }
    }

    pub fn target(&mut self, target: Target) -> &mut Self {
        self.target = target;
        self
    }

    pub fn stdin<T: Into<OwnedFd>>(&mut self, fd: T) -> &mut Self {
        self.stdin = Some(fd.into());
        self
    }

    pub fn stdout<T: Into<OwnedFd>>(&mut self, fd: T) -> &mut Self {
        self.stdout = Some(fd.into());
        self
    }

    pub fn stderr<T: Into<OwnedFd>>(&mut self, fd: T) -> &mut Self {
        self.stderr = Some(fd.into());
        self
    }

    pub fn spawn(&mut self) -> anyhow::Result<Child> {
        let pid = self.do_spawn()?;

        Ok(Child {
            pid,
            stdin: self.stdin.take(),
            stderr: self.stderr.take(),
            stdout: self.stdout.take(),
        })
    }

    fn do_spawn(&self) -> anyhow::Result<Option<libc::pid_t>> {
        let mut argv = ExecVec::default();
        // set prog name (argv[0])
        argv.push(CString::new("trampoline")?);
        type SpawnFn = dyn Fn(&SealedExecVec, &SealedExecVec);

        let spawn: Box<SpawnFn> = match &self.spawn_method {
            #[cfg(target_os = "linux")]
            SpawnMethod::FdExec => {
                let fd = linux::write_trampoline()?;
                Box::new(move |argv, envp| {
                    // not using nix crate here, as it would allocate args after fork, which will lead to crashes on systems
                    // where allocator is not fork+thread safe
                    unsafe { libc::fexecve(fd.as_raw_fd(), argv.as_ptr(), envp.as_ptr()) };
                    // if we're here then exec has failed
                    panic!("{}", std::io::Error::last_os_error());
                })
            }
            SpawnMethod::Exec => {
                let path = CString::new(
                    write_trampoline()?
                        .into_temp_path()
                        .keep()? // ensure the file is not auto cleaned in parent process
                        .as_os_str()
                        .to_str()
                        .ok_or_else(|| anyhow::format_err!("can't convert tmp file path"))?,
                )?;

                Box::new(move |argv, envp| {
                    // not using nix crate here, to avoid allocations post fork
                    unsafe { libc::execve(path.as_ptr(), argv.as_ptr(), envp.as_ptr()) };
                    // if we're here then exec has failed
                    panic!("{}", std::io::Error::last_os_error());
                })
            }
        };

        match &self.target {
            Target::Trampoline(f) => {
                let (library_path, symbol_name) =
                    match unsafe { crate::get_dl_path_raw(*f as *const libc::c_void) } {
                        (Some(p), Some(n)) => (p, n),
                        _ => return Err(anyhow::format_err!("can't read symbol pointer data")),
                    };
                argv.push(library_path);
                argv.push(symbol_name);
            }
            Target::ManualTrampoline(library_path, symbol_name) => {
                argv.push(library_path.clone());
                argv.push(symbol_name.clone());
            }
            Target::Fork(_) => todo!(),
            Target::Noop => return Ok(None),
        };

        let argv = argv.seal();

        let mut envp = ExecVec::default();

        if self.inherit_env {
            for (k, v) in std::env::vars() {
                envp.push(CString::new(format!("{k}={v}"))?);
            }
        }

        let envp = envp.seal();

        if let Fork::Parent(child_pid) = unsafe { crate::fork()? } {
            return Ok(Some(child_pid));
        }
        if let Some(fd) = &self.stdin {
            unsafe { libc::dup2(fd.as_raw_fd(), libc::STDIN_FILENO) };
        }

        if let Some(fd) = &self.stdout {
            unsafe { libc::dup2(fd.as_raw_fd(), libc::STDOUT_FILENO) };
        }

        if let Some(fd) = &self.stderr {
            unsafe { libc::dup2(fd.as_raw_fd(), libc::STDERR_FILENO) };
        }

        spawn(&argv, &envp);
        std::process::exit(1);
    }
}
impl Default for SpawnCfg {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Child {
    pid: Option<libc::pid_t>,
    pub stdin: Option<OwnedFd>,
    pub stderr: Option<OwnedFd>,
    pub stdout: Option<OwnedFd>,
}

impl Child {
    pub fn wait(self) -> anyhow::Result<WaitStatus> {
        let pid = match self.pid {
            Some(pid) => Pid::from_raw(pid),
            None => return Ok(WaitStatus::Exited(Pid::from_raw(0), 0)),
        };

        Ok(nix::sys::wait::waitpid(Some(pid), None)?)
    }
}

use std::{
    ffi::CString,
    fs::Permissions,
    io::{Seek, Write},
    os::unix::prelude::{AsRawFd, PermissionsExt},
    ptr,
};

use io_lifetimes::OwnedFd;
use nix::{sys::wait::WaitStatus, unistd::Pid};

use crate::{Fork, TRAMPOLINE_BIN};
