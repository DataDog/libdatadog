// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::shm_names;
use crate::platform::mem_handle::page_aligned_size;
use crate::platform::private_dir;
use crate::platform::shm_guard::{self, shm_owner_uid};
use crate::platform::{
    FileBackedHandle, MappedMem, MemoryHandle, NamedShmHandle, PlatformHandle, ShmHandle, ShmPath,
};
use io_lifetimes::OwnedFd;
use libc::off_t;
use nix::NixPath;
use nix::errno::Errno;
#[cfg(target_os = "linux")]
use nix::fcntl::{FallocateFlags, fallocate};
use nix::fcntl::{OFlag, open};
use nix::sys::mman::{self, MapFlags, ProtFlags, mmap, munmap};
use nix::sys::stat::Mode;
use nix::unistd::{Uid, fchown, ftruncate, unlink};
use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io;
use std::num::NonZeroUsize;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

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
    static VERIFIED: AtomicBool = AtomicBool::new(false);
    if VERIFIED.load(Ordering::Relaxed) {
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
            VERIFIED.store(true, Ordering::Relaxed);
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

#[cfg(test)]
thread_local! {
    /// Take the filesystem fallback and the `ftruncate` fallback on this thread, as a host
    /// without POSIX shared memory or `fallocate` would.
    pub(crate) static FORCE_FALLBACKS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn forced_fallbacks() -> bool {
    FORCE_FALLBACKS.with(|forced| forced.get())
}

#[cfg(not(test))]
fn forced_fallbacks() -> bool {
    false
}

fn shm_open<P: ?Sized + NixPath>(
    name: &P,
    flag: OFlag,
    mode: Mode,
) -> nix::Result<std::os::unix::io::OwnedFd> {
    let result = if forced_fallbacks() {
        Err(Errno::ENOSYS)
    } else {
        mman::shm_open(name, flag, mode)
    };
    result.or_else(|e| {
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

/// Create exclusively; [`super::shm_names`] handles replacement.
pub(crate) fn sys_create_exclusive(
    name: &CStr,
    mode: Mode,
) -> nix::Result<std::os::unix::io::OwnedFd> {
    shm_open(name, OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_RDWR, mode)
}

/// Open an existing segment for replacing it, refusing one another user could have created.
pub(crate) fn sys_open_existing(name: &CStr) -> nix::Result<std::os::unix::io::OwnedFd> {
    let fd = shm_open(name, OFlag::O_RDWR, Mode::empty())?;
    shm_guard::verify_owner(&fd, || name.to_string_lossy().into_owned())?;
    Ok(fd)
}

/// Device and inode identify the segment while its descriptor stays open.
pub(crate) fn sys_identity(fd: &impl AsRawFd) -> Option<shm_names::SegmentIdentity> {
    let stat = nix::sys::stat::fstat(fd.as_raw_fd()).ok()?;
    #[allow(clippy::unnecessary_cast)] // the field widths differ between platforms
    Some((stat.st_dev as u64, stat.st_ino as u64))
}

/// Nothing to stamp: a new file already has an identity of its own.
pub(crate) fn sys_stamp_identity(fd: &impl AsRawFd) -> Option<shm_names::SegmentIdentity> {
    sys_identity(fd)
}

pub(crate) fn sys_shm_unlink(name: &CStr) -> nix::Result<()> {
    shm_unlink(name)
}

pub fn shm_unlink<P: ?Sized + NixPath>(name: &P) -> nix::Result<()> {
    let result = if forced_fallbacks() {
        Err(Errno::ENOSYS)
    } else {
        mman::shm_unlink(name)
    };
    result.or_else(|e| {
        if e == Errno::ENOSYS || e == Errno::ENOTSUP || e == Errno::ENOENT {
            let path = fallback_path(name)?;
            unlink(path.as_c_str())
        } else {
            Err(e)
        }
    })
}

/// Address space reserved for every mapping, so that growing a segment never has to move it.
///
/// We assume any shared data to be below that hard limit. Just mapping it.
/// Use `[MappedMem::ensure_space()]` on fresh mappings to ensure they're accessible.
const MAPPING_RESERVED_SIZE: usize = 1 << 27;

pub(crate) fn mmap_handle<T: FileBackedHandle>(handle: T) -> io::Result<MappedMem<T>> {
    let fd = handle.get_shm().handle.as_owned_fd()?.as_fd();
    let Some(size) = NonZeroUsize::new(handle.get_shm().size) else {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "shared memory size not yet initialized",
        ));
    };
    // A segment that already exceeds the standard reservation keeps its own size as one: it
    // cannot grow in place beyond that, but it must at least be wholly mappable.
    let reserved = MAPPING_RESERVED_SIZE.max(page_aligned_size(size.get()));
    #[allow(clippy::unwrap_used)] // a max() with a non-zero constant is non-zero
    let reserve = NonZeroUsize::new(reserved).unwrap();
    Ok(MappedMem {
        ptr: unsafe {
            mmap(
                None,
                reserve,
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                fd,
                0,
            )?
        },
        mapped_len: reserved,
        // Only what the backing file holds is touchable; the rest of the reservation faults
        // until something commits it.
        usable: AtomicUsize::new(size.get().min(reserved)),
        mem: handle,
    })
}

pub(crate) fn munmap_handle<T: MemoryHandle>(mapped: &mut MappedMem<T>) {
    _ = unsafe { munmap(mapped.ptr, mapped.mapped_len) };
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
        // Predictable names may already exist; skip them without touching their segments.
        let fd = shm_names::create_anonymous(
            || {
                #[allow(clippy::unwrap_used)] // a formatted path contains no interior NUL
                CString::new(format!(
                    "/libdatadog-shm-{name}-{}-{}",
                    unsafe { libc::getpid() },
                    ANON_SHM_ID.fetch_add(1, Ordering::SeqCst)
                ))
                .unwrap()
            },
            Mode::S_IRUSR | Mode::S_IWUSR,
        )?;
        Ok(fd)
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
    pub fn create_mode_replacing(
        path: CString,
        size: usize,
        mode: Mode,
    ) -> io::Result<(NamedShmHandle, Vec<NamedShmHandle>)> {
        let (fd, shm_path, replaced) = shm_names::create_replacing(path.as_c_str(), mode, |fd| {
            // Allocate eagerly so a full /dev/shm returns ENOSPC here instead of SIGBUS later.
            // The ftruncate fallback is safe because this segment is fresh.
            #[cfg(target_os = "linux")]
            match if forced_fallbacks() {
                Err(nix::Error::ENOTSUP)
            } else {
                fallocate(fd.as_raw_fd(), FallocateFlags::empty(), 0, size as off_t)
            } {
                Err(nix::Error::EPERM | nix::Error::ENOSYS | nix::Error::ENOTSUP) => {
                    ftruncate(fd, size as off_t)?
                }
                Err(e) => return Err(e.into()),
                Ok(_) => {}
            }
            #[cfg(not(target_os = "linux"))]
            ftruncate(fd, size as off_t)?;
            if let Some(uid) = shm_owner_uid() {
                let _ = fchown(fd.as_raw_fd(), Some(Uid::from_raw(uid)), None);
            }
            Ok(())
        })?;
        let replaced = replaced
            .into_iter()
            .filter_map(|fd| {
                let file: File = fd.into();
                let size = file.metadata().ok()?.size() as usize;
                Some(Self::new(file.into(), None, size))
            })
            .collect();
        Ok((Self::new(fd, Some(shm_path), size), replaced))
    }

    pub fn open(path: &CStr) -> io::Result<NamedShmHandle> {
        let file: File = sys_open_existing(path)?.into();
        let size = file.metadata()?.size() as usize;
        let path = ShmPath {
            name: path.to_owned(),
            ownership: None,
        };
        Ok(Self::new(file.into(), Some(path), size))
    }

    fn new(fd: OwnedFd, path: Option<ShmPath>, size: usize) -> NamedShmHandle {
        NamedShmHandle {
            inner: ShmHandle {
                handle: fd.into(),
                size,
            },
            path: path.map(Box::new).into(),
        }
    }
}

impl<T: FileBackedHandle> MappedMem<T> {
    /// Pick up backing that somebody else committed, without committing any.
    ///
    /// `usable` is per-process: only this handle's own [`Self::ensure_space`] raises it, so a
    /// segment a peer grew stays invisible here until something asks. The backing file's
    /// length is the shared record of how far it has been grown, and `fstat` reads that
    /// without changing it - so this can be called on paths that must not allocate.
    ///
    /// Returns the usable length afterwards.
    pub fn refresh_size(&self) -> usize {
        if let Ok(fd) = self.mem.get_shm().handle.as_owned_fd() {
            if let Ok(stat) = nix::sys::stat::fstat(fd.as_raw_fd()) {
                let backed = (stat.st_size as usize).min(self.mapped_len);
                self.usable.fetch_max(backed, Ordering::AcqRel);
            }
        }
        self.get_size()
    }

    /// Back `expected_size` bytes of the reservation, leaving the mapping where it is, and
    /// report whether that many bytes are now usable.
    ///
    /// `false` means the segment is still too short - the request exceeds the reservation, or
    /// there was no space to allocate - and nothing may be written past what it already has.
    #[must_use = "a segment that could not be grown is still too short to write to"]
    pub fn ensure_space(&self, expected_size: usize) -> bool {
        if expected_size <= self.get_size() {
            return true;
        }
        let expected_size = page_aligned_size(expected_size);
        if expected_size > self.mapped_len {
            return false;
        }
        let Ok(fd) = self.mem.get_shm().handle.as_owned_fd() else {
            return false;
        };

        // From zero rather than from the current end: it costs nothing when the range is
        // already there, and it leaves no hole if somebody else grew the file meanwhile.
        #[cfg(target_os = "linux")]
        match fallocate(
            fd.as_raw_fd(),
            FallocateFlags::empty(),
            0,
            expected_size as off_t,
        ) {
            Ok(_) => {}
            // Filesystems without fallocate: `ftruncate` grows the file too, but it also
            // shrinks, so it must never be handed a size below what the file already has -
            // pages another process is holding would start faulting.
            Err(Errno::EPERM | Errno::ENOSYS | Errno::ENOTSUP) => {
                if !grow_with_ftruncate(fd, expected_size) {
                    return false;
                }
            }
            Err(_) => return false,
        }
        #[cfg(not(target_os = "linux"))]
        if !grow_with_ftruncate(fd, expected_size) {
            return false;
        }

        self.usable.fetch_max(expected_size, Ordering::AcqRel);
        true
    }
}

/// Extend a backing file to `size` without ever shortening it.
fn grow_with_ftruncate(fd: &OwnedFd, size: usize) -> bool {
    match nix::sys::stat::fstat(fd.as_raw_fd()) {
        Ok(stat) if (stat.st_size as usize) >= size => true,
        Ok(_) => ftruncate(fd, size as off_t).is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::FORCE_FALLBACKS;
    use crate::platform::{FileBackedHandle, NamedShmHandle};
    use std::ffi::CString;
    use std::io::Write;

    /// Replacement has to work the same through the filesystem fallback - where names live in
    /// a directory of our own rather than in the shm namespace - and on filesystems without
    /// `fallocate`, where sizing a segment is an `ftruncate` that could just as well shrink one.
    /// A fresh segment is the only kind that path may ever size.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn replacement_through_the_filesystem_and_ftruncate_fallbacks() {
        FORCE_FALLBACKS.with(|forced| forced.set(true));
        let path = CString::new(format!("/ddtest-fallback-{}", std::process::id())).unwrap();

        let old = NamedShmHandle::create(path.clone(), 3 * 4096).unwrap();
        let mut old = old.map().unwrap();
        _ = old.as_slice_mut().write(&[1, 2, 3]).unwrap();
        let reader = NamedShmHandle::open(&path).unwrap().map().unwrap();

        let (new, replaced) = NamedShmHandle::create_replacing(path.clone(), 4096).unwrap();
        let new = new.map().unwrap();
        assert_eq!(
            replaced.len(),
            1,
            "the fallback file is replaced, not adopted"
        );
        assert_eq!(&new.as_slice()[..3], &[0, 0, 0]);
        assert_eq!(
            reader.as_slice().len(),
            3 * 4096,
            "sizing the replacement must not have shrunk the old segment"
        );
        assert_eq!(&reader.as_slice()[..3], &[1, 2, 3]);
        assert_eq!(
            &NamedShmHandle::open(&path)
                .unwrap()
                .map()
                .unwrap()
                .as_slice()[..3],
            &[0, 0, 0],
            "the name refers to the replacement"
        );

        drop(old);
        assert!(
            NamedShmHandle::open(&path).is_ok(),
            "the replaced owner leaves the name"
        );
        drop(new);
        assert!(
            NamedShmHandle::open(&path).is_err(),
            "the current owner removes it"
        );
        FORCE_FALLBACKS.with(|forced| forced.set(false));
    }
}
