// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::platform::{
    FileBackedHandle, MappedMem, MemoryHandle, NamedShmHandle, PlatformHandle, ShmHandle, ShmPath,
};
use io_lifetimes::OwnedFd;
use libc::{chmod, off_t};
use nix::errno::Errno;
use nix::fcntl::{open, OFlag};
use nix::sys::mman::{self, mmap, munmap, MapFlags, ProtFlags};
use nix::sys::stat::Mode;
use nix::unistd::{ftruncate, mkdir, unlink};
use nix::NixPath;
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::num::NonZeroUsize;
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicI32, Ordering};

fn shm_open<P: ?Sized + NixPath>(
    name: &P,
    flag: OFlag,
    mode: Mode,
) -> nix::Result<std::os::unix::io::OwnedFd> {
    mman::shm_open(name, flag, mode).or_else(|e| {
        // This can happen on AWS lambda
        if e == Errno::ENOSYS || e == Errno::ENOTSUP {
            // The path has a leading slash
            let path = name.with_nix_path(|cstr| {
                let mut path = "/tmp/libdatadog".to_string().into_bytes();
                path.extend_from_slice(cstr.to_bytes_with_nul());
                unsafe { CString::from_vec_with_nul_unchecked(path) }
            })?;
            open(path.as_c_str(), flag, mode)
                .or_else(|e| {
                    if (flag & OFlag::O_CREAT) == OFlag::O_CREAT && e == Errno::ENOENT {
                        #[allow(clippy::unwrap_used)]
                        mkdir(c"/tmp/libdatadog", Mode::from_bits(0o1777).unwrap())?;
                        // work around umask(2).
                        unsafe { chmod(c"/tmp/libdatadog".as_ptr(), 0o1777) };
                        open(path.as_c_str(), flag, mode)
                    } else {
                        Err(e)
                    }
                })
                .map(|fd| unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) })
        } else {
            Err(e)
        }
    })
}

pub fn shm_unlink<P: ?Sized + NixPath>(name: &P) -> nix::Result<()> {
    mman::shm_unlink(name).or_else(|e| {
        if e == Errno::ENOSYS || e == Errno::ENOTSUP {
            unlink(name)
        } else {
            Err(e)
        }
    })
}

pub(crate) fn mmap_handle<T: FileBackedHandle>(handle: T) -> io::Result<MappedMem<T>> {
    let fd = handle.get_shm().handle.as_owned_fd()?;
    if let Some(size) = NonZeroUsize::new(handle.get_shm().size) {
        Ok(MappedMem {
            ptr: unsafe {
                mmap(
                    None,
                    size,
                    ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                    MapFlags::MAP_SHARED,
                    Some(fd),
                    0,
                )?
            },
            mem: handle,
        })
    } else {
        Err(io::Error::other("Size of handle used for mmap() is zero. When used for shared memory this may originate from race conditions between creation and truncation of the shared memory file."))
    }
}

pub(crate) fn munmap_handle<T: MemoryHandle>(mapped: &mut MappedMem<T>) {
    unsafe {
        _ = munmap(mapped.ptr, mapped.mem.get_size());
    }
}

static ANON_SHM_ID: AtomicI32 = AtomicI32::new(0);

impl ShmHandle {
    #[cfg(target_os = "linux")]
    fn open_anon_shm(name: &str) -> anyhow::Result<OwnedFd> {
        if let Ok(memfd) = memfd::MemfdOptions::default().create(name) {
            Ok(memfd.into_file().into())
        } else {
            Self::open_anon_shm_generic(name)
        }
    }

    fn open_anon_shm_generic(name: &str) -> anyhow::Result<OwnedFd> {
        let path = format!(
            "/libdatadog-shm-{name}-{}-{}",
            unsafe { libc::getpid() },
            ANON_SHM_ID.fetch_add(1, Ordering::SeqCst)
        );
        let result = shm_open(
            path.as_bytes(),
            OFlag::O_CREAT | OFlag::O_RDWR,
            Mode::empty(),
        );
        _ = shm_unlink(path.as_bytes());
        Ok(result?)
    }

    #[cfg(not(target_os = "linux"))]
    fn open_anon_shm(name: &str) -> anyhow::Result<OwnedFd> {
        Self::open_anon_shm_generic(name)
    }

    pub fn new(size: usize) -> anyhow::Result<ShmHandle> {
        Self::new_named(size, "anon-handle")
    }

    pub fn new_named(size: usize, name: &str) -> anyhow::Result<ShmHandle> {
        let fd = Self::open_anon_shm(name)?;
        let handle: PlatformHandle<OwnedFd> = fd.into();
        ftruncate(handle.as_owned_fd()?, size as off_t)?;
        Ok(ShmHandle { handle, size })
    }
}

impl NamedShmHandle {
    pub fn create(path: CString, size: usize) -> io::Result<NamedShmHandle> {
        Self::create_mode(path, size, Mode::S_IWUSR | Mode::S_IRUSR)
    }

    pub fn create_mode(path: CString, size: usize, mode: Mode) -> io::Result<NamedShmHandle> {
        let fd = shm_open(path.as_bytes(), OFlag::O_CREAT | OFlag::O_RDWR, mode)?;
        ftruncate(&fd, size as off_t)?;
        Self::new(fd, Some(path), size)
    }

    pub fn open(path: &CStr) -> io::Result<NamedShmHandle> {
        let file: File = shm_open(path, OFlag::O_RDWR, Mode::empty())?.into();
        let size = file.metadata()?.size() as usize;
        Self::new(file.into(), None, size)
    }

    fn new(fd: OwnedFd, path: Option<CString>, size: usize) -> io::Result<NamedShmHandle> {
        Ok(NamedShmHandle {
            inner: ShmHandle {
                handle: fd.into(),
                size,
            },
            path: path.map(|path| ShmPath { name: path }),
        })
    }
}

impl<T: FileBackedHandle + From<MappedMem<T>>> MappedMem<T> {
    pub fn ensure_space(&mut self, expected_size: usize) {
        if expected_size <= self.mem.get_shm().size {
            return;
        }

        // SAFETY: we'll overwrite the original memory later
        let mut handle: T = unsafe { std::ptr::read(self) }.into();
        _ = handle.resize(expected_size);
        #[allow(clippy::unwrap_used)]
        unsafe {
            std::ptr::write(self, handle.map().unwrap())
        };
    }
}

impl Drop for ShmPath {
    fn drop(&mut self) {
        _ = shm_unlink(self.name.as_c_str());
    }
}
