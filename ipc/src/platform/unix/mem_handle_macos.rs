// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::platform::{
    FileBackedHandle, MappedMem, MemoryHandle, NamedShmHandle, ShmHandle, ShmPath,
};
use libc::off_t;
use nix::errno::Errno;
use nix::fcntl::OFlag;
use nix::sys::mman::{mmap, munmap, shm_open, shm_unlink, MapFlags, ProtFlags};
use nix::sys::stat::Mode;
use nix::unistd::ftruncate;
use std::ffi::{CStr, CString};
use std::io;
use std::num::NonZeroUsize;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

const MAPPING_MAX_SIZE: usize = 1 << 17; // 128 MiB ought to be enough for everybody?
const NOT_COMMITTED: usize = 1 << (usize::BITS - 1);

pub(crate) fn mmap_handle<T: FileBackedHandle>(mut handle: T) -> io::Result<MappedMem<T>> {
    let shm = handle.get_shm_mut();
    let fd = shm.handle.as_owned_fd()?;
    if shm.size & NOT_COMMITTED != 0 {
        shm.size &= !NOT_COMMITTED;
        let page_size = NonZeroUsize::try_from(page_size::get()).unwrap();
        unsafe {
            let ptr = mmap(
                None,
                page_size,
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                Some(fd),
                (MAPPING_MAX_SIZE - usize::from(page_size)) as off_t,
            )?;
            if shm.size == 0 {
                shm.size = *(ptr as *mut usize);
            } else {
                *(ptr as *mut usize) = shm.size;
            }
            _ = munmap(ptr, usize::from(page_size));
        }
    }

    Ok(MappedMem {
        ptr: unsafe {
            mmap(
                None,
                NonZeroUsize::new(shm.size).unwrap(),
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                Some(fd),
                0,
            )?
        },
        mem: handle,
    })
}

pub(crate) fn munmap_handle<T: MemoryHandle>(mapped: &MappedMem<T>) {
    unsafe {
        _ = munmap(mapped.ptr, mapped.mem.get_size());
    }
}

static ANON_SHM_ID: AtomicI32 = AtomicI32::new(0);

impl ShmHandle {
    pub fn new(size: usize) -> anyhow::Result<ShmHandle> {
        let path = format!(
            "ddshm-anon-{}-{}",
            unsafe { libc::getpid() },
            ANON_SHM_ID.fetch_add(1, Ordering::SeqCst)
        );
        let fd = shm_open(
            path.as_bytes(),
            OFlag::O_CREAT | OFlag::O_RDWR,
            Mode::empty(),
        )?;
        ftruncate(&fd, MAPPING_MAX_SIZE as off_t)?;
        _ = shm_unlink(path.as_bytes());
        Ok(ShmHandle {
            handle: fd.into(),
            size: size | NOT_COMMITTED,
        })
    }

    pub fn new_named(size: usize, _name: &str) -> anyhow::Result<ShmHandle> {
        Self::new(size)
    }
}
fn path_slice(path: &CStr) -> &[u8] {
    assert_eq!(path.to_bytes()[0], b'/');
    &path.to_bytes()[1..]
}

impl NamedShmHandle {
    pub fn create(path: CString, size: usize) -> io::Result<NamedShmHandle> {
        Self::create_mode(path, size, Mode::S_IWUSR | Mode::S_IRUSR)
    }

    pub fn create_mode(path: CString, size: usize, mode: Mode) -> io::Result<NamedShmHandle> {
        let fd = shm_open(path_slice(&path), OFlag::O_CREAT | OFlag::O_RDWR, mode)?;
        let truncate = ftruncate(&fd, MAPPING_MAX_SIZE as off_t);
        if let Err(error) = truncate {
            // ignore if already exists
            if error != Errno::EINVAL {
                truncate?;
            }
        }
        Self::new(fd, Some(path), size)
    }

    pub fn open(path: &CStr) -> io::Result<NamedShmHandle> {
        let fd = shm_open(path_slice(path), OFlag::O_RDWR, Mode::empty())?;
        Self::new(fd, None, 0)
    }

    fn new(fd: OwnedFd, path: Option<CString>, size: usize) -> io::Result<NamedShmHandle> {
        Ok(NamedShmHandle {
            inner: ShmHandle {
                handle: fd.into(),
                size: size | NOT_COMMITTED,
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
        if expected_size > MAPPING_MAX_SIZE - page_size::get() {
            panic!(
                "Tried to allocate {} bytes for shared mapping (limit: {} bytes)",
                expected_size,
                MAPPING_MAX_SIZE - page_size::get()
            );
        }

        // SAFETY: we'll overwrite the original memory later
        let mut handle: T = unsafe { std::ptr::read(self) }.into();

        let page_size = NonZeroUsize::try_from(page_size::get()).unwrap();
        unsafe {
            _ = handle.set_mapping_size(expected_size);
            let ptr = mmap(
                None,
                page_size,
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                Some(handle.get_shm().handle.as_owned_fd().unwrap()),
                (MAPPING_MAX_SIZE - usize::from(page_size)) as off_t,
            )
            .unwrap();
            // AtomicUsize::from_ptr() is still unstable
            let size = &*(ptr as *const AtomicUsize);
            size.fetch_max(handle.get_size(), Ordering::SeqCst);
            _ = munmap(ptr, usize::from(page_size));
        }

        unsafe { std::ptr::write(self, handle.map().unwrap()) };
    }
}

impl Drop for ShmPath {
    fn drop(&mut self) {
        _ = shm_unlink(path_slice(&self.name));
    }
}
