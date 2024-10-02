// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::handles::{HandlesTransport, TransferHandles};
use crate::platform::{mmap_handle, munmap_handle, OwnedFileHandle, PlatformHandle};
use serde::{Deserialize, Serialize};
#[cfg(all(unix, not(target_os = "macos")))]
use std::os::unix::prelude::AsRawFd;
use std::{ffi::CString, io};
#[cfg(feature = "tiny-bytes")]
use tinybytes::UnderlyingBytes;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ShmHandle {
    pub(crate) handle: PlatformHandle<OwnedFileHandle>,
    pub(crate) size: usize,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct AnonHandle {
    pub(crate) size: usize,
}

pub struct MappedMem<T>
where
    T: MemoryHandle,
{
    #[cfg(unix)]
    pub(crate) ptr: *mut libc::c_void,
    #[cfg(windows)]
    pub(crate) ptr: *mut winapi::ctypes::c_void,
    pub(crate) mem: T,
}

pub(crate) struct ShmPath {
    pub(crate) name: CString,
}

pub struct NamedShmHandle {
    pub(crate) inner: ShmHandle,
    pub(crate) path: Option<ShmPath>,
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
    #[cfg(all(unix, not(target_os = "macos")))]
    fn resize(&mut self, size: usize) -> anyhow::Result<()> {
        unsafe {
            self.set_mapping_size(size)?;
        }
        nix::unistd::ftruncate(
            self.get_shm().handle.as_raw_fd(),
            self.get_shm().size as libc::off_t,
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

impl<T: MemoryHandle> AsRef<[u8]> for MappedMem<T> {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
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
        munmap_handle(self);
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

impl From<ShmHandle> for PlatformHandle<OwnedFileHandle> {
    fn from(shm: ShmHandle) -> Self {
        shm.handle
    }
}

unsafe impl<T> Sync for MappedMem<T> where T: FileBackedHandle {}
unsafe impl<T> Send for MappedMem<T> where T: FileBackedHandle {}

#[cfg(feature = "tiny-bytes")]
impl UnderlyingBytes for MappedMem<ShmHandle> {}

#[cfg(test)]
mod tests {
    use crate::platform::{FileBackedHandle, NamedShmHandle, ShmHandle};
    use std::ffi::CString;
    use std::io::Write;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_anon_shm() {
        let shm = ShmHandle::new(5).unwrap();
        let mut mapped = shm.map().unwrap();
        _ = mapped.as_slice_mut().write(&[1, 2, 3, 4, 5]).unwrap();
        mapped.ensure_space(100000);
        assert!(mapped.as_slice().len() >= 100000);
        let mut exp = vec![0u8; mapped.as_slice().len()];
        _ = (&mut exp[..5]).write(&[1, 2, 3, 4, 5]).unwrap();
        assert_eq!(mapped.as_slice(), exp.as_slice());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_named_shm() {
        let path = CString::new("/foo").unwrap();
        let shm = NamedShmHandle::create(path.clone(), 5).unwrap();
        let mut mapped = shm.map().unwrap();
        _ = mapped.as_slice_mut().write(&[1, 2, 3, 4, 5]).unwrap();
        mapped.ensure_space(100000);
        assert!(mapped.as_slice().len() >= 100000);

        let other = NamedShmHandle::open(&path).unwrap().map().unwrap();
        let mut exp = vec![0u8; other.as_slice().len()];
        _ = (&mut exp[..5]).write(&[1, 2, 3, 4, 5]).unwrap();
        assert_eq!(other.as_slice(), exp.as_slice());
    }
}
