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
            state: FLockState::Locked,
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
    pub fn rw_lock<P: AsRef<Path>>(path: P) -> io::Result<Self> {
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
            Err(err) => {
                this.close();
                Err(err.into())
            }
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

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Read, Write},
        time::Duration,
    };

    use tempfile::tempdir;

    use crate::{
        assert_child_exit,
        fork::{
            fork_fn,
            tests::{prevent_concurrent_tests, set_default_child_panic_handler},
        },
        ipc::platform::ForkableUnixHandlePair,
    };

    use super::FLock;

    #[test]
    #[ignore]
    fn test_file_locking_works_as_expected() {
        let _g = prevent_concurrent_tests();
        let d = tempdir().unwrap();
        let lock_path = d.path().join("file.lock");
        let pair = ForkableUnixHandlePair::new().unwrap();

        let pid = unsafe {
            fork_fn((&pair, &lock_path), |(pair, lock_path)| {
                set_default_child_panic_handler();
                let _l = FLock::rw_lock(lock_path).unwrap();
                let mut c = pair.remote().into_instance().unwrap();

                c.write_all(&[0]).unwrap(); // signal readiness
                let mut buf = [0; 10];
                assert!(c.read(&mut buf).unwrap() > 0); // wait for signal to closepp

                std::process::exit(0); // exit without explicitly freeing
            })
        }
        .unwrap();

        let mut c = unsafe { pair.local() }.into_instance().unwrap();
        let mut buf = [0; 10];
        c.set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        // wait for child to signal its ready
        assert!(c.read(&mut buf).unwrap() > 0);

        // must fail, as file is locked by another process
        assert!(lock_path.exists());
        let err = FLock::rw_lock(&lock_path).err().unwrap();
        assert_eq!(io::ErrorKind::WouldBlock, err.kind());

        c.write_all(&[0]).unwrap(); // signal child to shut down

        assert_child_exit!(pid);
        assert!(!lock_path.exists());
        // must succeed as no other process is holding the lock
        let lock = FLock::rw_lock(&lock_path).unwrap();
        assert!(lock_path.exists());
        drop(lock);
        assert!(!lock_path.exists());
    }
}
