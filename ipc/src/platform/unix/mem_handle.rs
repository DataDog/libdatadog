// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::platform::{
    FileBackedHandle, MappedMem, MemoryHandle, NamedShmHandle, PlatformHandle, ShmHandle, ShmPath,
};
use io_lifetimes::OwnedFd;
use libc::off_t;
use nix::fcntl::OFlag;
use nix::sys::mman::{mmap, munmap, shm_open, shm_unlink, MapFlags, ProtFlags};
use nix::sys::stat::Mode;
use nix::unistd::ftruncate;
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::num::NonZeroUsize;
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicI32, Ordering};

pub(crate) fn mmap_handle<T: FileBackedHandle>(handle: T) -> io::Result<MappedMem<T>> {
    let fd = handle.get_shm().handle.as_owned_fd()?;
    Ok(MappedMem {
        ptr: unsafe {
            mmap(
                None,
                NonZeroUsize::new(handle.get_shm().size).unwrap(),
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                Some(fd),
                0,
            )?
        },
        mem: handle,
    })
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
        unsafe { std::ptr::write(self, handle.map().unwrap()) };
    }
}

impl Drop for ShmPath {
    fn drop(&mut self) {
        _ = shm_unlink(self.name.as_c_str());
    }
}
