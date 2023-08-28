use crate::platform::{
    FileBackedHandle, MappedMem, NamedShmHandle, PlatformHandle, ShmHandle, ShmPath,
};
use libc::{getpid, off_t};
use nix::fcntl::OFlag;
use nix::sys::mman::{mmap, munmap, shm_open, shm_unlink, MapFlags, ProtFlags};
use nix::sys::stat::Mode;
use nix::unistd::ftruncate;
#[cfg(not(target_os = "linux"))]
use nix::unistd::getpid;
use std::fs::File;
use std::io;
use std::num::NonZeroUsize;
use std::os::unix::fs::MetadataExt;
use std::os::unix::prelude::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
use std::sync::atomic::{AtomicI32, Ordering};

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

pub(crate) fn munmap_handle<T: FileBackedHandle>(mapped: &mut MappedMem<T>) {
    unsafe {
        _ = munmap(mapped.ptr, mapped.mem.get_size());
    }
}

static ANON_SHM_ID: AtomicI32 = AtomicI32::default();

impl ShmHandle {
    #[cfg(target_os = "linux")]
    fn open_anon_shm() -> anyhow::Result<RawFd> {
        Ok(memfd::MemfdOptions::default()
            .create("anon-shm-handle")?
            .into_raw_fd())
    }

    #[cfg(not(target_os = "linux"))]
    fn open_anon_shm() -> anyhow::Result<RawFd> {
        let path = format!(
            "/libdatadog-shm-anon-{}-{}",
            unsafe { getpid() },
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
        Self::new(fd, Some(path), size)
    }

    pub fn open(path: CString) -> io::Result<NamedShmHandle> {
        let fd = shm_open(path.as_bytes(), OFlag::O_RDWR, Mode::empty())?;
        let file: File = unsafe { OwnedFd::from_raw_fd(fd) }.into();
        let size = file.metadata()?.size() as usize;
        Self::new(file.into_raw_fd(), None, size)
    }

    fn new(fd: RawFd, path: Option<CString>, size: usize) -> io::Result<NamedShmHandle> {
        Ok(NamedShmHandle {
            inner: ShmHandle {
                handle: unsafe { PlatformHandle::from_raw_fd(fd) },
                size,
            },
            path: path.map(|path| ShmPath { name: path }),
        })
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

impl Drop for ShmPath {
    fn drop(&mut self) {
        _ = shm_unlink(self.name.as_c_str());
    }
}
