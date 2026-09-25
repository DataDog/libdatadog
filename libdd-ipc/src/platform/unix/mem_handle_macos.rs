// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::platform::mem_handle::page_aligned_size;
use crate::platform::shm_guard::{self, shm_owner_uid};
use crate::platform::{
    FileBackedHandle, MappedMem, MemoryHandle, NamedShmHandle, ShmHandle, ShmPath,
};
use libc::off_t;
use nix::errno::Errno;
use nix::fcntl::OFlag;
use nix::sys::mman::{mmap, munmap, shm_open, shm_unlink, MapFlags, ProtFlags};
use nix::sys::stat::Mode;
use nix::unistd::{fchown, ftruncate, Uid};
use std::ffi::{CStr, CString};
use std::io;
use std::num::NonZeroUsize;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::io::AsRawFd;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

const MAPPING_MAX_SIZE: usize = 1 << 27; // 128 MiB ought to be enough for everybody?
const NOT_COMMITTED: usize = 1 << (usize::BITS - 1);

/// The largest a segment's contents may get: the reservation, less the page holding its length.
fn usable_max() -> usize {
    MAPPING_MAX_SIZE - page_size::get()
}

/// The word carrying the segment's committed length, in its own last page.
///
/// macOS can neither grow a mapping in place nor `fallocate`, so every segment is `ftruncate`d
/// to the whole reservation when it is created and the length actually in use is kept inside
/// the segment, where every process that maps it can read it and raise it.
///
/// # Safety
/// `ptr` must be the base of a `MAPPING_MAX_SIZE`-byte mapping of such a segment.
unsafe fn committed_len<'a>(ptr: NonNull<libc::c_void>) -> &'a AtomicUsize {
    AtomicUsize::from_ptr(
        ptr.as_ptr()
            .cast::<u8>()
            .add(MAPPING_MAX_SIZE - page_size::get())
            .cast(),
    )
}

pub(crate) fn mmap_handle<T: FileBackedHandle>(mut handle: T) -> io::Result<MappedMem<T>> {
    let declared = handle.get_shm().size;
    #[allow(clippy::unwrap_used)] // a non-zero constant
    let reserve = NonZeroUsize::new(MAPPING_MAX_SIZE).unwrap();

    // The whole reservation in one mapping, which puts the length word inside it too. The
    // backing object is already this long, so nothing here can fault, and the mapping never
    // has to move again.
    let ptr = {
        let fd = handle.get_shm().handle.as_owned_fd()?.as_fd();
        unsafe {
            mmap(
                None,
                reserve,
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                fd,
                0,
            )?
        }
    };

    // SAFETY: the mapping is MAPPING_MAX_SIZE long, as just requested above.
    let committed = unsafe { committed_len(ptr) };
    let usable = if declared & NOT_COMMITTED == 0 {
        declared.min(usable_max())
    } else {
        match declared & !NOT_COMMITTED {
            // Freshly opened: the creator recorded how much of the segment it is using.
            // Clamped, because that word lives in shared memory like everything else here.
            0 => committed.load(Ordering::Acquire).min(usable_max()),
            // Freshly created: publish our own length, without lowering anybody else's.
            size => {
                let size = size.min(usable_max());
                committed.fetch_max(size, Ordering::AcqRel);
                size
            }
        }
    };

    // Handle transiently not yet assigned size the same than a non-existing mapping
    if usable == 0 {
        unsafe { _ = munmap(ptr, MAPPING_MAX_SIZE) };
        return Err(io::Error::other(
            "shared memory mapping size not yet committed",
        ));
    }

    handle.get_shm_mut().size = usable;
    Ok(MappedMem {
        ptr,
        mapped_len: MAPPING_MAX_SIZE,
        usable: AtomicUsize::new(usable),
        mem: handle,
    })
}

pub(crate) fn munmap_handle<T: MemoryHandle>(mapped: &MappedMem<T>) {
    unsafe {
        // The whole reservation, not the part in use: that is what was mapped.
        _ = munmap(mapped.ptr, mapped.mapped_len);
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
        let fd = shm_open_exclusive(path.as_bytes(), Mode::S_IRUSR | Mode::S_IWUSR, || {
            path.clone()
        })?;
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
/// Open a segment we intend to own, refusing one another user got to first.
///
/// `O_EXCL` is what makes the difference: without it `O_CREAT` silently adopts an existing
/// segment and ignores `mode`, so a pre-planted one would be used as if we had made it. With
/// it, pre-existence becomes visible and can be checked - and a segment that is legitimately
/// ours already (left behind by an earlier sidecar, since shm outlives the process) is still
/// adopted, so no state is lost across restarts.
fn shm_open_exclusive<F>(name: &[u8], mode: Mode, display: F) -> nix::Result<OwnedFd>
where
    F: FnOnce() -> String,
{
    match shm_open(name, OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_RDWR, mode) {
        Ok(fd) => Ok(fd),
        Err(Errno::EEXIST) => {
            let fd = shm_open(name, OFlag::O_RDWR, mode)?;
            shm_guard::verify_owner(&fd, display)?;
            Ok(fd)
        }
        Err(e) => Err(e),
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
        let fd = shm_open_exclusive(path_slice(&path), mode, || {
            path.to_string_lossy().into_owned()
        })?;
        let truncate = ftruncate(&fd, MAPPING_MAX_SIZE as off_t);
        if let Err(error) = truncate {
            // ignore if already exists
            if error != Errno::EINVAL {
                truncate?;
            }
        }
        if let Some(uid) = shm_owner_uid() {
            let _ = fchown(fd.as_raw_fd(), Some(Uid::from_raw(uid)), None);
        }
        Self::new(fd, Some(path), size)
    }

    pub fn open(path: &CStr) -> io::Result<NamedShmHandle> {
        let fd = shm_open(path_slice(path), OFlag::O_RDWR, Mode::empty())?;
        // A reader is the more exposed side: it maps whatever is under the name and trusts the
        // contents. Check the descriptor before mapping it.
        shm_guard::verify_owner(&fd, || path.to_string_lossy().into_owned())?;
        Self::new(fd, None, 0)
    }

    /// Unlink the SHM name from the filesystem without unmapping existing mappings.
    pub fn unlink(&self) {
        let _ = self.path.take(); // Drop of Box<ShmPath> calls shm_unlink exactly once
    }

    fn new(fd: OwnedFd, path: Option<CString>, size: usize) -> io::Result<NamedShmHandle> {
        Ok(NamedShmHandle {
            inner: ShmHandle {
                handle: fd.into(),
                size: size | NOT_COMMITTED,
            },
            path: path.map(|path| Box::new(ShmPath { name: path })).into(),
        })
    }
}

pub(crate) fn unlink_shm_name(name: &CStr) {
    _ = shm_unlink(path_slice(name));
}

impl<T: FileBackedHandle> MappedMem<T> {
    /// Pick up backing that somebody else committed, without committing any.
    ///
    /// `usable` is per-process: only this handle's own [`Self::ensure_space`] raises it, so a
    /// segment a peer grew stays invisible here until something asks. The length word in the
    /// segment's own last page is the shared record of how far it has been grown, and reading
    /// it changes nothing - so this can be called on paths that must not allocate.
    ///
    /// Returns the usable length afterwards.
    pub fn refresh_size(&self) -> usize {
        // SAFETY: this mapping is MAPPING_MAX_SIZE long; see `mmap_handle`.
        let committed = unsafe { committed_len(self.ptr) }.load(Ordering::Acquire);
        self.usable
            .fetch_max(committed.min(usable_max()), Ordering::AcqRel);
        self.get_size()
    }

    /// Raise the segment's committed length, leaving the mapping where it is, and report
    /// whether `expected_size` bytes are now usable.
    ///
    /// Nothing is allocated here: the backing object was `ftruncate`d to the whole reservation
    /// when it was created, so growth is only a matter of agreeing how much of it is in use.
    ///
    /// `false` means the request exceeds what a segment can hold, and nothing may be written
    /// past what it already has.
    #[must_use = "a segment that could not be grown is still too short to write to"]
    pub fn ensure_space(&self, expected_size: usize) -> bool {
        if expected_size <= self.get_size() {
            return true;
        }
        let expected_size = page_aligned_size(expected_size);
        if expected_size > usable_max() {
            return false;
        }
        // SAFETY: this mapping is MAPPING_MAX_SIZE long; see `mmap_handle`.
        unsafe { committed_len(self.ptr) }.fetch_max(expected_size, Ordering::AcqRel);
        self.usable.fetch_max(expected_size, Ordering::AcqRel);
        true
    }
}

impl ShmHandle {
    /// Refresh the size of the shared memory segment
    /// No-op on macOS: every segment is `ftruncate`d to `MAPPING_MAX_SIZE` and carries its
    /// committed length in its own last page (see [`mmap_handle`]).
    pub(crate) fn limit_size_to_backing(&mut self) -> io::Result<()> {
        Ok(())
    }

    pub fn adjust_to_file_size(&mut self) -> io::Result<()> {
        self.size = NOT_COMMITTED;
        Ok(())
    }
}

impl Drop for ShmPath {
    fn drop(&mut self) {
        _ = shm_unlink(path_slice(self.name.as_c_str()));
    }
}
