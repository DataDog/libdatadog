// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::platform::private_dir;
use crate::platform::shm_guard::{self, shm_owner_uid};
use crate::platform::{
    FileBackedHandle, MappedMem, MemoryHandle, NamedShmHandle, PlatformHandle, ShmHandle, ShmPath,
};
use io_lifetimes::OwnedFd;
use libc::off_t;
use nix::errno::Errno;
#[cfg(target_os = "linux")]
use nix::fcntl::{fallocate, FallocateFlags};
use nix::fcntl::{open, OFlag};
use nix::sys::mman::{self, mmap, munmap, MapFlags, ProtFlags};
use nix::sys::stat::Mode;
use nix::unistd::{fchown, ftruncate, unlink, Uid};
use nix::NixPath;
use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io;
use std::num::NonZeroUsize;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::OnceLock;

/// Directory for the filesystem fallback below.
///
/// One per euid, and private to it: the segment names are predictable, so a directory other
/// users can write to would let any of them create a file where ours is expected and be handed
/// whatever the sidecar writes into it
fn fallback_dir() -> CString {
    // SAFETY: geteuid() takes no arguments and cannot fail.
    let euid = unsafe { libc::geteuid() };
    #[allow(clippy::unwrap_used)] // a formatted uid contains no interior NUL
    CString::new(format!("/tmp/libdatadog-{euid}")).unwrap()
}

fn fallback_path<P: ?Sized + NixPath>(name: &P) -> nix::Result<CString> {
    name.with_nix_path(|cstr| {
        let mut path = fallback_dir().into_bytes();
        path.extend_from_slice(cstr.to_bytes_with_nul());
        unsafe { CString::from_vec_with_nul_unchecked(path) }
    })
}

/// Vet the fallback directory before the first use of it in this process.
///
/// It has to happen before *any* open, not just one that finds the directory missing: an
/// attacker who pre-creates `/tmp/libdatadog-<victim euid>` as their own 0777 directory leaves
/// nothing for a create to trip over, and readers never create at all.
///
/// But it only has to happen *once*. The property being checked is that the directory is owned
/// by this euid, is writable by nobody else, and sits under a parent that is sticky or not
/// others-writable - and once that holds, no other user can chmod, rename, replace or unlink
/// it. Only this process or root could invalidate it afterwards, and root is not a threat this
/// can defend against in the first place. So this is a first-use check, and later opens pay
/// nothing.
///
/// Only success is remembered. Latching a failure would mean that a host where somebody had
/// squatted the name could never recover once cleaned up, short of a restart.
///
/// `creating` distinguishes the two callers, and it matters: [`private_dir::ensure`] may
/// discard and recreate the directory to repair it, which would take the segments a writer is
/// serving from with it. Only the path that is about to create may do that; a reader verifies
/// and nothing more.
fn check_fallback_dir(creating: bool) -> nix::Result<()> {
    static VERIFIED: OnceLock<()> = OnceLock::new();
    if VERIFIED.get().is_some() {
        return Ok(());
    }

    let dir = fallback_dir();
    let path = Path::new(OsStr::from_bytes(dir.as_bytes()));
    let checked = if creating {
        private_dir::ensure(path, 0o700)
    } else {
        private_dir::verify(path)
    };

    match checked {
        Ok(_) => {
            let _ = VERIFIED.set(());
            Ok(())
        }
        Err(e) => {
            // A missing directory is not a problem worth reporting on a read: there is simply
            // nothing stored yet, and the open below will say so.
            if e.kind() != io::ErrorKind::NotFound {
                // An Errno carries only a number, so the explanation - which uid owns the
                // directory, what mode it has - would be lost right here. Say it while we can.
                tracing::error!("Cannot use the shared memory fallback directory: {e}");
            }
            Err(Errno::from_raw(e.raw_os_error().unwrap_or(libc::EPERM)))
        }
    }
}

fn shm_open<P: ?Sized + NixPath>(
    name: &P,
    flag: OFlag,
    mode: Mode,
) -> nix::Result<std::os::unix::io::OwnedFd> {
    mman::shm_open(name, flag, mode).or_else(|e| {
        // This can happen on AWS lambda
        if e == Errno::ENOSYS || e == Errno::ENOTSUP || e == Errno::ENOENT || e == Errno::EACCES {
            // The path has a leading slash
            let path = fallback_path(name)?;
            check_fallback_dir((flag & OFlag::O_CREAT) == OFlag::O_CREAT)?;
            let flag = flag | OFlag::O_NOFOLLOW;
            open(path.as_c_str(), flag, mode)
                .map(|fd| unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) })
        } else {
            Err(e)
        }
    })
}

/// Open a segment we intend to own, refusing one another user got to first.
///
/// `O_EXCL` is what makes the difference: without it `O_CREAT` silently adopts an existing
/// segment and ignores `mode`, so a pre-planted one would be used as if we had made it. With
/// it, pre-existence becomes visible and can be checked - and a segment that is legitimately
/// ours already (left behind by an earlier sidecar, since shm outlives the process) is still
/// adopted, so no state is lost across restarts.
fn shm_open_exclusive(name: &CStr, mode: Mode) -> nix::Result<std::os::unix::io::OwnedFd> {
    match shm_open(name, OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_RDWR, mode) {
        Ok(fd) => Ok(fd),
        Err(Errno::EEXIST) => {
            let fd = shm_open(name, OFlag::O_RDWR, mode)?;
            shm_guard::verify_owner(&fd, || name.to_string_lossy().into_owned())?;
            Ok(fd)
        }
        Err(e) => Err(e),
    }
}

pub fn shm_unlink<P: ?Sized + NixPath>(name: &P) -> nix::Result<()> {
    mman::shm_unlink(name).or_else(|e| {
        if e == Errno::ENOSYS || e == Errno::ENOTSUP || e == Errno::ENOENT {
            let path = fallback_path(name)?;
            unlink(path.as_c_str())
        } else {
            Err(e)
        }
    })
}

pub(crate) fn mmap_handle<T: FileBackedHandle>(handle: T) -> io::Result<MappedMem<T>> {
    let fd = handle.get_shm().handle.as_owned_fd()?.as_fd();
    if let Some(size) = NonZeroUsize::new(handle.get_shm().size) {
        Ok(MappedMem {
            ptr: unsafe {
                mmap(
                    None,
                    size,
                    ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                    MapFlags::MAP_SHARED,
                    fd,
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
    _ = unsafe { munmap(mapped.ptr, mapped.mem.get_size()) };
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
        // Exclusive, and 0600: the name embeds only pid and a counter, so another user can
        // pre-create it. The segment is unlinked immediately afterwards, but that does not help
        // if we adopted somebody else's to begin with.
        #[allow(clippy::unwrap_used)] // a formatted path contains no interior NUL
        let cpath = CString::new(path.as_bytes()).unwrap();
        let result = shm_open_exclusive(cpath.as_c_str(), Mode::S_IRUSR | Mode::S_IWUSR);
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

    /// Clamp a declared size to what the descriptor can actually back.
    ///
    /// The size on a handle that arrived over IPC is whatever the peer put on the wire; the
    /// descriptor is the only part of it the kernel vouches for. Mapping more than the file
    /// holds means `SIGBUS` on first touch of the tail, and in thread mode the "sidecar" is a
    /// thread inside the PHP master - so a worker that has dropped privileges would be taking
    /// down a root process, and reading past the end of its own segment would be reading that
    /// process's heap.
    ///
    /// `min`, not assignment: a peer declaring *less* than the file holds is legitimate and its
    /// intent is kept.
    pub(crate) fn limit_size_to_backing(&mut self) -> io::Result<()> {
        let fd = self.handle.as_owned_fd()?;
        let backing = nix::sys::stat::fstat(fd.as_raw_fd())?.st_size as usize;
        self.size = self.size.min(backing);
        Ok(())
    }

    /// Refresh the size of the shared memory segment
    pub fn adjust_to_file_size(&mut self) -> io::Result<()> {
        let fd = self.handle.as_owned_fd()?;
        self.size = nix::sys::stat::fstat(fd.as_raw_fd())?.st_size as usize;
        Ok(())
    }
}

impl NamedShmHandle {
    pub fn create(path: CString, size: usize) -> io::Result<NamedShmHandle> {
        Self::create_mode(path, size, Mode::S_IWUSR | Mode::S_IRUSR)
    }

    pub fn create_mode(path: CString, size: usize, mode: Mode) -> io::Result<NamedShmHandle> {
        let fd = shm_open_exclusive(path.as_c_str(), mode)?;
        // Try to use fallocate on Linux to eagerly commit pages: if /dev/shm is full we get ENOSPC
        // here (recoverable) rather than SIGBUS mid-execution when a worker writes a slot.
        #[cfg(target_os = "linux")]
        match fallocate(fd.as_raw_fd(), FallocateFlags::empty(), 0, size as off_t) {
            Err(nix::Error::EPERM | nix::Error::ENOSYS | nix::Error::ENOTSUP) => {
                ftruncate(&fd, size as off_t)?
            }
            Err(e) => return Err(e.into()),
            Ok(_) => {}
        }
        #[cfg(not(target_os = "linux"))]
        ftruncate(&fd, size as off_t)?;
        if let Some(uid) = shm_owner_uid() {
            let _ = fchown(fd.as_raw_fd(), Some(Uid::from_raw(uid)), None);
        }
        Self::new(fd, Some(path), size)
    }

    pub fn open(path: &CStr) -> io::Result<NamedShmHandle> {
        let fd = shm_open(path, OFlag::O_RDWR, Mode::empty())?;
        // A reader is the more exposed side: it maps whatever is under the name and trusts the
        // contents. Check the descriptor before mapping it.
        shm_guard::verify_owner(&fd, || path.to_string_lossy().into_owned())?;
        let file: File = fd.into();
        let size = file.metadata()?.size() as usize;
        Self::new(file.into(), None, size)
    }

    /// Unlink the SHM file from the filesystem without unmapping it.
    pub fn unlink(&self) {
        let _ = self.path.take(); // Drop of Box<ShmPath> calls shm_unlink exactly once
    }

    fn new(fd: OwnedFd, path: Option<CString>, size: usize) -> io::Result<NamedShmHandle> {
        Ok(NamedShmHandle {
            inner: ShmHandle {
                handle: fd.into(),
                size,
            },
            path: path.map(|path| Box::new(ShmPath { name: path })).into(),
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
