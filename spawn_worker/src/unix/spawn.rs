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
    ViaFnPtr(extern "C" fn()),
    Manual(CString, CString),
    Fork(fn()),
    Noop,
}

pub struct SpawnCfg {
    stdin: Option<OwnedFd>,
    stderr: Option<OwnedFd>,
    stdout: Option<OwnedFd>,
    spawn_method: SpawnMethod,
    target: Target,
    env: Vec<(ffi::OsString, ffi::OsString)>,
}

impl SpawnCfg {
    pub fn from_env<E: IntoIterator<Item = (ffi::OsString, ffi::OsString)>>(env: E) -> Self {
        Self {
            stdin: None,
            stdout: None,
            stderr: None,
            target: Target::Noop,
            spawn_method: Default::default(),
            env: env.into_iter().collect(),
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
        let mut argv = ExecVec::empty();
        // set prog name (argv[0])
        argv.push(CString::new("trampoline")?);

        match &self.target {
            Target::ViaFnPtr(f) => {
                let (library_path, symbol_name) =
                    match unsafe { crate::get_dl_path_raw(*f as *const libc::c_void) } {
                        (Some(p), Some(n)) => (p, n),
                        _ => return Err(anyhow::format_err!("can't read symbol pointer data")),
                    };
                argv.push(library_path);
                argv.push(symbol_name);
            }
            Target::Manual(library_path, symbol_name) => {
                argv.push(library_path.clone());
                argv.push(symbol_name.clone());
            }
            Target::Fork(_) => todo!(),
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

        type SpawnFn = dyn Fn(&ExecVec, &ExecVec);

        // build and allocate final exec fn and its dependencies
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

        // no allocations in the child process should happen by this point for maximum safety
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
    env,
    ffi::{self, CString, OsString},
    fs::Permissions,
    io::{Seek, Write},
    os::unix::prelude::{AsRawFd, OsStringExt, PermissionsExt},
    ptr,
};

use io_lifetimes::OwnedFd;
use nix::{sys::wait::WaitStatus, unistd::Pid};

use crate::{Fork, TRAMPOLINE_BIN};
