// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::shm_names;
use crate::platform::mem_handle::page_aligned_size;
use crate::platform::shm_guard::{self, shm_owner_uid};
use crate::platform::{
    FileBackedHandle, MappedMem, MemoryHandle, NamedShmHandle, ShmHandle, ShmPath,
};
use libc::off_t;
use nix::fcntl::OFlag;
use nix::sys::mman::{MapFlags, ProtFlags, mmap, munmap, shm_open, shm_unlink};
use nix::sys::stat::Mode;
use nix::unistd::{Uid, fchown, ftruncate};
use std::ffi::{CStr, CString};
use std::io;
use std::num::NonZeroUsize;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::io::AsRawFd;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};

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
    unsafe {
        AtomicUsize::from_ptr(
            ptr.as_ptr()
                .cast::<u8>()
                .add(MAPPING_MAX_SIZE - page_size::get())
                .cast(),
        )
    }
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

    // The creator has not committed the mapping size yet.
    if usable == 0 {
        unsafe { _ = munmap(ptr, MAPPING_MAX_SIZE) };
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
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
        // Predictable names may already exist; skip them without touching their segments.
        let fd = shm_names::create_anonymous(
            || {
                #[allow(clippy::unwrap_used)] // a formatted path contains no interior NUL
                CString::new(format!(
                    "/ddshm-anon-{}-{}",
                    unsafe { libc::getpid() },
                    ANON_SHM_ID.fetch_add(1, Ordering::SeqCst)
                ))
                .unwrap()
            },
            Mode::S_IRUSR | Mode::S_IWUSR,
        )?;
        ftruncate(&fd, MAPPING_MAX_SIZE as off_t)?;
        Ok(ShmHandle {
            handle: fd.into(),
            size: size | NOT_COMMITTED,
        })
    }

    pub fn new_named(size: usize, _name: &str) -> anyhow::Result<ShmHandle> {
        Self::new(size)
    }
}

/// Create exclusively; [`super::shm_names`] handles replacement.
pub(crate) fn sys_create_exclusive(name: &CStr, mode: Mode) -> nix::Result<OwnedFd> {
    shm_open(
        path_slice(name),
        OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_RDWR,
        mode,
    )
}

/// Open an existing segment for replacing it, refusing one another user could have created.
pub(crate) fn sys_open_existing(name: &CStr) -> nix::Result<OwnedFd> {
    let fd = shm_open(path_slice(name), OFlag::O_RDWR, Mode::empty())?;
    shm_guard::verify_owner(&fd, || name.to_string_lossy().into_owned())?;
    Ok(fd)
}

/// macOS SHM has no device/inode identity. Store a token after the committed length in the
/// reserved metadata page, outside the payload.
const IDENTITY_OFFSET: usize = size_of::<usize>();

/// Access the identity token only if the backing is large enough to hold the metadata page.
fn with_identity_token<R>(fd: &impl AsFd, f: impl FnOnce(&AtomicU64) -> R) -> Option<R> {
    let stat = nix::sys::stat::fstat(fd.as_fd().as_raw_fd()).ok()?;
    if (stat.st_size as u64) < MAPPING_MAX_SIZE as u64 {
        return None;
    }
    let page = page_size::get();
    #[allow(clippy::unwrap_used)] // a page size is non-zero
    let len = NonZeroUsize::new(page).unwrap();
    // SAFETY: a fresh shared mapping of one page within the object's size, as just checked.
    let ptr = unsafe {
        mmap(
            None,
            len,
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
            MapFlags::MAP_SHARED,
            fd,
            (MAPPING_MAX_SIZE - page) as off_t,
        )
    }
    .ok()?;
    // SAFETY: in bounds of the page just mapped, and suitably aligned.
    let result = f(unsafe { &*ptr.as_ptr().cast::<u8>().add(IDENTITY_OFFSET).cast() });
    unsafe { _ = munmap(ptr, page) };
    Some(result)
}

static IDENTITY_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Give a segment just created and sized a token of its own; see [`sys_identity`].
pub(crate) fn sys_stamp_identity(fd: &impl AsFd) -> Option<shm_names::SegmentIdentity> {
    // Unique among the processes alive at a time, and never 0: the pid is not.
    let token = (u64::from(std::process::id()) << 32)
        | u64::from(IDENTITY_COUNTER.fetch_add(1, Ordering::Relaxed));
    with_identity_token(fd, |word| word.store(token, Ordering::Release))?;
    Some((0, token))
}

/// Read the creator's identity token.
pub(crate) fn sys_identity(fd: &impl AsFd) -> Option<shm_names::SegmentIdentity> {
    let token = with_identity_token(fd, |word| word.load(Ordering::Acquire))?;
    (token != 0).then_some((0, token))
}

pub(crate) fn sys_shm_unlink(name: &CStr) -> nix::Result<()> {
    shm_unlink(path_slice(name))
}

/// Use the same spelling for create, open and unlink: macOS distinguishes the leading slash.
fn path_slice(path: &CStr) -> &[u8] {
    let bytes = path.to_bytes();
    bytes.strip_prefix(b"/").unwrap_or(bytes)
}

impl NamedShmHandle {
    pub fn create_mode_replacing(
        path: CString,
        size: usize,
        mode: Mode,
    ) -> io::Result<(NamedShmHandle, Vec<NamedShmHandle>)> {
        let (fd, shm_path, replaced) = shm_names::create_replacing(path.as_c_str(), mode, |fd| {
            // Fresh, so sized exactly once: this is the whole reservation, see `mmap_handle`.
            ftruncate(fd, MAPPING_MAX_SIZE as off_t)?;
            if let Some(uid) = shm_owner_uid() {
                let _ = fchown(fd.as_raw_fd(), Some(Uid::from_raw(uid)), None);
            }
            Ok(())
        })?;
        let replaced = replaced
            .into_iter()
            .map(|fd| Self::new(fd, None, 0))
            .collect();
        Ok((Self::new(fd, Some(shm_path), size), replaced))
    }

    pub fn open(path: &CStr) -> io::Result<NamedShmHandle> {
        let fd = sys_open_existing(path)?;
        let path = ShmPath {
            name: path.to_owned(),
            ownership: None,
        };
        Ok(Self::new(fd, Some(path), 0))
    }

    fn new(fd: OwnedFd, path: Option<ShmPath>, size: usize) -> NamedShmHandle {
        NamedShmHandle {
            inner: ShmHandle {
                handle: fd.into(),
                size: size | NOT_COMMITTED,
            },
            path: path.map(Box::new).into(),
        }
    }
}

impl<T: FileBackedHandle> MappedMem<T> {
    /// macOS POSIX SHM does not support `mmap(MAP_PRIVATE)`. Make the view COW in place.
    pub(crate) fn make_private(&self) -> io::Result<()> {
        unsafe extern "C" {
            static mach_task_self_: libc::mach_port_t;
            fn mach_vm_protect(
                task: libc::mach_port_t,
                address: libc::mach_vm_address_t,
                size: libc::mach_vm_size_t,
                set_maximum: libc::boolean_t,
                protection: libc::vm_prot_t,
            ) -> libc::kern_return_t;
        }
        const VM_PROT_COPY: libc::vm_prot_t = 0x10;
        // SAFETY: the full range belongs to this mapping and remains at the same address.
        let result = unsafe {
            mach_vm_protect(
                mach_task_self_,
                self.ptr.as_ptr() as libc::mach_vm_address_t,
                self.mapped_len as libc::mach_vm_size_t,
                0,
                VM_PROT_COPY | libc::VM_PROT_READ | libc::VM_PROT_WRITE,
            )
        };
        if result == libc::KERN_SUCCESS {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "mach_vm_protect failed: {result}"
            )))
        }
    }

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
