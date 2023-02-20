// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::{
    io,
    os::unix::prelude::RawFd,
    path::{Path, PathBuf},
};

use nix::{
    fcntl::{FcntlArg, OFlag},
    sys::stat::Mode,
    NixPath,
};

pub enum FLockState {
    Open,
    Locked,
}
#[must_use]
/// FLock can acquire exclusive lock between processes
///
/// A lock with a specific path is held per process.
/// Calling lock 2nd time in the same process after it has been successfully acquired
/// will not prevent 2nd lock from being acquired.
///
/// Lock is automatically released when process exits
pub struct FLock {
    fd: RawFd,
    state: FLockState,
    path: PathBuf,
}

impl FLock {
    fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let fd = nix::fcntl::open(
            path.as_ref(),
            OFlag::O_CREAT | OFlag::O_NOCTTY | OFlag::O_RDWR,
            Mode::S_IRUSR
                | Mode::S_IRGRP
                | Mode::S_IROTH
                | Mode::S_IWUSR
                | Mode::S_IWGRP
                | Mode::S_IWOTH,
        )?;
        Ok(FLock {
            fd,
            path: path.as_ref().to_path_buf(),
            state: FLockState::Open,
        })
    }

    fn close(&mut self) {
        if self.fd >= 0 {
            unsafe {
                let _ = libc::close(self.fd);
            }
            self.fd = -1;
        }
    }

    fn unlink(&mut self) -> io::Result<()> {
        self.path.as_os_str().with_nix_path(|p| unsafe {
            let _ = libc::unlink(p.as_ptr());
        })?;
        Ok(())
    }

    /// Locks file at path for writing using fcntl
    /// once Self is dropped, the lock is released
    pub fn try_rw_lock<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let mut this = Self::open(&path)?;
        let lock = libc::flock {
            l_type: libc::F_WRLCK as i16,
            l_whence: 0,
            l_start: 0,
            l_len: 0,
            l_pid: 0,
        };

        match nix::fcntl::fcntl(this.fd, FcntlArg::F_SETLK(&lock)) {
            Ok(_) => {
                this.state = FLockState::Locked;
                Ok(this)
            }
            Err(err) => Err(err.into()),
        }
    }
}

impl Drop for FLock {
    fn drop(&mut self) {
        match self.state {
            FLockState::Open => {
                self.close();
            }
            FLockState::Locked => {
                let _ = self.unlink();
                self.close();
            }
        }
    }
}
