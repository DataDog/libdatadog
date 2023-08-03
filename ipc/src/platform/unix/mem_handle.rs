// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::handles::{HandlesTransport, TransferHandles};
use crate::platform::PlatformHandle;
use io_lifetimes::OwnedFd;
use libc::off_t;
use nix::fcntl::OFlag;
use nix::sys::mman::{mmap, munmap, shm_open, shm_unlink, MapFlags, ProtFlags};
use nix::sys::stat::Mode;
use nix::unistd::ftruncate;
#[cfg(not(target_os = "linux"))]
use nix::unistd::getpid;
use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::fs::File;
use std::io;
use std::num::NonZeroUsize;
use std::os::unix::fs::MetadataExt;
use std::os::unix::prelude::{AsRawFd, FromRawFd, IntoRawFd, RawFd};

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ShmHandle {
    handle: PlatformHandle<OwnedFd>,
    size: usize,
}

#[derive(Debug)]
pub struct AnonHandle {
    size: usize,
}

pub struct MappedMem<T>
where
    T: MemoryHandle,
{
    ptr: *mut libc::c_void,
    mem: T,
}

struct ShmPath {
    name: CString,
}

pub struct NamedShmHandle {
    inner: ShmHandle,
    path: Option<ShmPath>,
}

impl NamedShmHandle {
    pub fn get_path(&self) -> &[u8] {
        if let Some(ref shm_path) = &self.path {
            shm_path.name.as_bytes()
        } else {
            b""
        }
    }
}

fn page_aligned_size(size: usize) -> usize {
    let page_size = page_size::get();
    // round up to nearest page
    ((size - 1) & !(page_size - 1)) + page_size
}

pub trait MemoryHandle {
    fn get_size(&self) -> usize;
}

impl MemoryHandle for AnonHandle {
    fn get_size(&self) -> usize {
        self.size
    }
}

impl<T> MemoryHandle for T
where
    T: FileBackedHandle,
{
    fn get_size(&self) -> usize {
        self.get_shm().size
    }
}

pub trait FileBackedHandle
where
    Self: Sized,
{
    fn map(self) -> io::Result<MappedMem<Self>>;
    fn get_shm(&self) -> &ShmHandle;
    fn get_shm_mut(&mut self) -> &mut ShmHandle;
    fn resize(&mut self, size: usize) -> anyhow::Result<()> {
        unsafe {
            self.set_mapping_size(size)?;
        }
        ftruncate(
            self.get_shm().handle.as_raw_fd(),
            self.get_shm().size as off_t,
        )?;
        Ok(())
    }
    /// # Safety
    /// Calling function needs to ensure it's appropriately resized
    unsafe fn set_mapping_size(&mut self, size: usize) -> anyhow::Result<()> {
        if size == 0 {
            anyhow::bail!("Cannot allocate mapping of size zero");
        }

        self.get_shm_mut().size = page_aligned_size(size);
        Ok(())
    }
}

fn mmap_handle<T: FileBackedHandle>(handle: T) -> io::Result<MappedMem<T>> {
    let fd: RawFd = handle.get_shm().handle.as_raw_fd();
    Ok(MappedMem {
        ptr: unsafe {
            mmap(
                None,
                NonZeroUsize::new(handle.get_shm().size).unwrap(),
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                fd,
                0,
            )?
        },
        mem: handle,
    })
}

impl FileBackedHandle for ShmHandle {
    fn map(self) -> io::Result<MappedMem<ShmHandle>> {
        mmap_handle(self)
    }

    fn get_shm(&self) -> &ShmHandle {
        self
    }
    fn get_shm_mut(&mut self) -> &mut ShmHandle {
        self
    }
}

impl FileBackedHandle for NamedShmHandle {
    fn map(self) -> io::Result<MappedMem<NamedShmHandle>> {
        mmap_handle(self)
    }

    fn get_shm(&self) -> &ShmHandle {
        &self.inner
    }
    fn get_shm_mut(&mut self) -> &mut ShmHandle {
        &mut self.inner
    }
}

impl ShmHandle {
    #[cfg(target_os = "linux")]
    fn open_anon_shm() -> anyhow::Result<RawFd> {
        Ok(memfd::MemfdOptions::default()
            .create("anon-shm-handle")?
            .into_raw_fd())
    }

    #[cfg(not(target_os = "linux"))]
    fn open_anon_shm() -> anyhow::Result<RawFd> {
        let path = format!("/libdatadog-shm-anon-{}", getpid());
        let result = shm_open(
            path.as_bytes(),
            OFlag::O_CREAT | OFlag::O_RDWR,
            Mode::empty(),
        );
        _ = shm_unlink(path.as_bytes());
        Ok(result?)
    }

    pub fn new(size: usize) -> anyhow::Result<ShmHandle> {
        let fd = Self::open_anon_shm()?;
        let handle = unsafe { PlatformHandle::from_raw_fd(fd) };
        ftruncate(fd, size as off_t)?;
        Ok(ShmHandle { handle, size })
    }
}

impl NamedShmHandle {
    pub fn create(path: CString, size: usize) -> io::Result<NamedShmHandle> {
        let fd = shm_open(
            path.as_bytes(),
            OFlag::O_CREAT | OFlag::O_RDWR,
            Mode::S_IWUSR
                | Mode::S_IRUSR
                | Mode::S_IRGRP
                | Mode::S_IWGRP
                | Mode::S_IROTH
                | Mode::S_IWOTH,
        )?;
        ftruncate(fd, size as off_t)?;
        Self::new(fd, path, size)
    }

    pub fn open(path: CString) -> io::Result<NamedShmHandle> {
        let fd = shm_open(path.as_bytes(), OFlag::O_RDWR, Mode::empty())?;
        let file: File = unsafe { OwnedFd::from_raw_fd(fd) }.into();
        let size = file.metadata()?.size() as usize;
        Self::new(file.into_raw_fd(), path, size)
    }

    fn new(fd: RawFd, path: CString, size: usize) -> io::Result<NamedShmHandle> {
        Ok(NamedShmHandle {
            inner: ShmHandle {
                handle: unsafe { PlatformHandle::from_raw_fd(fd) },
                size,
            },
            path: Some(ShmPath { name: path }),
        })
    }
}

impl<T: MemoryHandle> MappedMem<T> {
    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr as *const u8, self.mem.get_size()) }
    }

    pub fn as_slice_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr as *mut u8, self.mem.get_size()) }
    }

    pub fn get_size(&self) -> usize {
        self.mem.get_size()
    }
}

impl<T: FileBackedHandle + From<MappedMem<T>>> MappedMem<T> {
    pub fn ensure_space(self, expected_size: usize) -> MappedMem<T> {
        if expected_size <= self.mem.get_shm().size {
            return self;
        }

        let mut handle: T = self.into();
        _ = handle.resize(expected_size);
        handle.map().unwrap()
    }
}

impl MappedMem<NamedShmHandle> {
    pub fn get_path(&self) -> &[u8] {
        self.mem.get_path()
    }
}

impl<T: FileBackedHandle> From<MappedMem<T>> for ShmHandle {
    fn from(handle: MappedMem<T>) -> ShmHandle {
        ShmHandle {
            handle: handle.mem.get_shm().handle.clone(),
            size: handle.mem.get_shm().size,
        }
    }
}

impl From<MappedMem<NamedShmHandle>> for NamedShmHandle {
    fn from(mut handle: MappedMem<NamedShmHandle>) -> NamedShmHandle {
        NamedShmHandle {
            path: handle.mem.path.take(),
            inner: handle.into(),
        }
    }
}

impl<T> Drop for MappedMem<T>
where
    T: MemoryHandle,
{
    fn drop(&mut self) {
        unsafe {
            _ = munmap(self.ptr, self.mem.get_size());
        }
    }
}

impl Drop for ShmPath {
    fn drop(&mut self) {
        _ = shm_unlink(self.name.as_bytes());
    }
}

impl TransferHandles for ShmHandle {
    fn move_handles<Transport: HandlesTransport>(
        &self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        self.handle.move_handles(transport)
    }

    fn receive_handles<Transport: HandlesTransport>(
        &mut self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        self.handle.receive_handles(transport)
    }
}

impl From<ShmHandle> for PlatformHandle<OwnedFd> {
    fn from(shm: ShmHandle) -> Self {
        shm.handle
    }
}

unsafe impl<T> Sync for MappedMem<T> where T: FileBackedHandle {}
unsafe impl<T> Send for MappedMem<T> where T: FileBackedHandle {}
