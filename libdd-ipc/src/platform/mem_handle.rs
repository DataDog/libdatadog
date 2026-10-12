// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::AtomicOptionBox;
use crate::handles::{HandlesTransport, TransferHandles};
use crate::platform::{OwnedFileHandle, PlatformHandle, mmap_handle, munmap_handle};
#[cfg(feature = "tiny-bytes")]
use libdd_tinybytes::UnderlyingBytes;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{ffi::CString, io, ptr::NonNull};

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

/// A mapping of a shared-memory segment, at an address that never changes.
pub struct MappedMem<T>
where
    T: MemoryHandle,
{
    #[cfg(unix)]
    pub(crate) ptr: NonNull<libc::c_void>,
    #[cfg(windows)]
    pub(crate) ptr: NonNull<std::ffi::c_void>,
    /// Exactly what was handed to `mmap`/`MapViewOfFile`, and what its unmap gets back.
    /// Fixed for the life of the mapping.
    pub(crate) mapped_len: usize,
    /// The backed prefix of the reservation: the part that may actually be touched.
    ///
    /// Only ever grows. Touching past it faults rather than failing - `SIGBUS` on the tail of
    /// a file-backed mapping, an access violation on reserved-but-uncommitted Windows pages -
    /// so this, never `mapped_len`, bounds every slice handed out.
    pub(crate) usable: AtomicUsize,
    pub(crate) mem: T,
}

pub(crate) struct ShmPath {
    pub(crate) name: CString,
    #[cfg(unix)]
    pub(crate) ownership: Option<crate::platform::NameOwnership>,
    /// Kept open by readers and owner alike; see `ShmIndex`.
    #[cfg(windows)]
    pub(crate) index: crate::platform::ShmIndex,
    /// Section suffix, present only for the owner allowed to clear the index.
    #[cfg(windows)]
    pub(crate) owned: Option<u64>,
}

#[cfg(unix)]
impl Drop for ShmPath {
    fn drop(&mut self) {
        if let Some(ownership) = &self.ownership {
            ownership.release(&self.name);
        }
    }
}

pub struct NamedShmHandle {
    /// Drop before `inner` so the open descriptor prevents identity reuse during unlink.
    pub(crate) path: AtomicOptionBox<ShmPath>,
    pub(crate) inner: ShmHandle,
}

impl NamedShmHandle {
    /// # Safety
    /// Must not be called concurrently with `unlink()`.
    pub unsafe fn get_path(&self) -> &[u8] {
        unsafe {
            match self.path.as_ref() {
                Some(shm_path) => shm_path.name.to_bytes(),
                None => b"",
            }
        }
    }

    /// Create a fresh segment at `path`, replacing any segment already there.
    ///
    /// Existing mappings stay valid. Use [`Self::create_replacing`] to mark them obsolete.
    pub fn create(path: CString, size: usize) -> io::Result<NamedShmHandle> {
        Self::create_replacing(path, size).map(|(created, _)| created)
    }

    /// As [`Self::create`], with the given permissions.
    #[cfg(unix)]
    pub fn create_mode(
        path: CString,
        size: usize,
        mode: nix::sys::stat::Mode,
    ) -> io::Result<NamedShmHandle> {
        Self::create_mode_replacing(path, size, mode).map(|(created, _)| created)
    }

    /// As [`Self::create`], also returning handles to the segments that were replaced.
    ///
    /// Initialize the new segment before marking the old ones obsolete so readers can switch.
    #[cfg(unix)]
    pub fn create_replacing(
        path: CString,
        size: usize,
    ) -> io::Result<(NamedShmHandle, Vec<NamedShmHandle>)> {
        use nix::sys::stat::Mode;
        Self::create_mode_replacing(path, size, Mode::S_IWUSR | Mode::S_IRUSR)
    }

    /// Remove the name if this handle owns it and it still refers to this segment.
    /// Existing mappings stay valid.
    pub fn unlink(&self) {
        let _ = self.path.take();
    }
}

pub(crate) fn page_aligned_size(size: usize) -> usize {
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

impl MappedMem<NamedShmHandle> {
    /// Remove the name so new openers get `ENOENT`, if it still refers to this segment.
    /// Existing mappings remain valid.
    pub fn unlink(&self) {
        self.mem.unlink();
    }
}

impl<T: MemoryHandle> MappedMem<T> {
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `usable` bytes from the base are mapped and backed. It only ever grows, so
        // the length read here stays valid for as long as the borrow does.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr().cast(), self.get_size()) }
    }

    pub fn as_slice_mut(&mut self) -> &mut [u8] {
        let len = self.get_size();
        // SAFETY: as above, and `&mut self` is the caller's guarantee of exclusivity within
        // this process.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr().cast(), len) }
    }

    /// How much of the mapping is backed and safe to touch.
    pub fn get_size(&self) -> usize {
        self.usable.load(Ordering::Acquire)
    }
}

impl<T: MemoryHandle> AsRef<[u8]> for MappedMem<T> {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl MappedMem<NamedShmHandle> {
    /// # Safety
    /// Must not be called concurrently with `unlink()`.
    pub unsafe fn get_path(&self) -> &[u8] {
        unsafe { self.mem.get_path() }
    }
}

impl<T: FileBackedHandle> From<MappedMem<T>> for ShmHandle {
    fn from(handle: MappedMem<T>) -> ShmHandle {
        ShmHandle {
            handle: handle.mem.get_shm().handle.clone(),
            // What the mapping actually has backed, which is what a peer can safely map in
            // turn - not the size this handle was created with.
            size: handle.get_size(),
        }
    }
}

impl From<MappedMem<NamedShmHandle>> for NamedShmHandle {
    fn from(handle: MappedMem<NamedShmHandle>) -> NamedShmHandle {
        let path = handle.mem.path.take().into();
        NamedShmHandle {
            path,
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
    fn copy_handles<Transport: HandlesTransport>(
        &self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        self.handle.copy_handles(transport)
    }

    fn receive_handles<Transport: HandlesTransport>(
        &mut self,
        transport: Transport,
    ) -> Result<(), Transport::Error> {
        self.handle.receive_handles(transport)?;

        if let Err(e) = self.limit_size_to_backing() {
            tracing::error!("Could not size shared memory against its backing file: {e}");
            self.size = 0;
        }
        Ok(())
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
    #[cfg(unix)]
    #[cfg_attr(miri, ignore)]
    fn detaching_after_fork_preserves_the_parent_mapping() {
        use crate::platform::lock_shm;
        use std::sync::atomic::{AtomicU64, Ordering};

        let size = 2 * page_size::get();
        let mapped = ShmHandle::new(size).unwrap().map().unwrap();
        let base = mapped.as_slice().as_ptr();
        let reserved = mapped.mapped_len;
        let first = unsafe { &*base.cast::<AtomicU64>() };
        let last = unsafe { &*base.add(size - size_of::<AtomicU64>()).cast::<AtomicU64>() };
        first.store(11, Ordering::Relaxed);
        last.store(22, Ordering::Relaxed);

        // Inherit a held lock too: detachment in the child must reclaim it.
        let _guard = lock_shm();
        // SAFETY: the child only detaches its mapping, writes atomics, and calls _exit.
        match unsafe { libc::fork() } {
            -1 => panic!("fork failed"),
            0 => {
                let _guard = lock_shm();
                let mut ok = mapped.make_private().is_ok();
                if ok {
                    ok = mapped.as_slice().as_ptr() == base
                        && mapped.mapped_len == reserved
                        && mapped.get_size() == size
                        && first.load(Ordering::Relaxed) == 11
                        && last.load(Ordering::Relaxed) == 22;
                    first.store(u64::MAX, Ordering::Relaxed);
                    last.store(u64::MAX, Ordering::Relaxed);
                }
                unsafe { libc::_exit(if ok { 0 } else { 1 }) }
            }
            pid => {
                let mut status = 0;
                assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
                assert!(libc::WIFEXITED(status));
                assert_eq!(libc::WEXITSTATUS(status), 0);
                assert_eq!(first.load(Ordering::Relaxed), 11);
                assert_eq!(last.load(Ordering::Relaxed), 22);
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_anon_shm() {
        let shm = ShmHandle::new(5).unwrap();
        let mut mapped = shm.map().unwrap();
        _ = mapped.as_slice_mut().write(&[1, 2, 3, 4, 5]).unwrap();
        assert!(mapped.ensure_space(100000));
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
        assert!(mapped.ensure_space(100000));
        assert!(mapped.as_slice().len() >= 100000);

        let other = NamedShmHandle::open(&path).unwrap().map().unwrap();
        let mut exp = vec![0u8; other.as_slice().len()];
        _ = (&mut exp[..5]).write(&[1, 2, 3, 4, 5]).unwrap();
        assert_eq!(other.as_slice(), exp.as_slice());
    }

    /// A handle's size arrives over IPC and is whatever the peer put there; only its
    /// descriptor is vouched for by the kernel. Mapping more than the file holds would
    /// `SIGBUS` on the tail - and in thread mode that tail is inside the PHP master, which may
    /// be root while the peer is a worker that dropped privileges.
    #[test]
    #[cfg(not(any(target_os = "macos", windows)))]
    #[cfg_attr(miri, ignore)]
    fn test_shm_size_is_clamped_to_its_backing() {
        let mut shm = ShmHandle::new(4096).unwrap();

        // Stands in for a peer that declared far more than it allocated.
        shm.size = 1 << 30;
        shm.limit_size_to_backing().unwrap();
        assert_eq!(
            shm.size, 4096,
            "a declared size beyond the backing file must be clamped to it"
        );

        // Declaring less than the file holds is legitimate - the writer may have used only
        // part of it - and must be left alone.
        shm.size = 128;
        shm.limit_size_to_backing().unwrap();
        assert_eq!(shm.size, 128, "an under-declared size must be preserved");
    }

    /// The clamp has to happen when the handle is received, not when a consumer remembers to
    /// ask: a caller that forgets would map past the end of the peer's segment.
    #[test]
    // Same gating as the test above: the clamp is a deliberate no-op where segments are a fixed
    // size that carries its committed length internally.
    #[cfg(not(any(target_os = "macos", windows)))]
    #[cfg_attr(miri, ignore)]
    fn received_shm_size_is_clamped_to_its_backing() {
        use crate::handles::TransferHandles;
        use crate::platform::FdSource;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        let shm = ShmHandle::new(4096).unwrap();
        // Stands in for the descriptor arriving over SCM_RIGHTS.
        let raw = shm.handle.as_owned_fd().unwrap().as_raw_fd();
        let sent = unsafe { OwnedFd::from_raw_fd(nix::unistd::dup(raw).unwrap()) };

        // ... paired with a size the peer made up.
        let mut received = shm.clone();
        received.size = 1 << 30;

        let mut source = FdSource::new(vec![sent]);
        received.receive_handles(&mut source).unwrap();

        assert_eq!(
            received.size, 4096,
            "receiving a handle must clamp its declared size to the backing file"
        );
    }

    /// Creating a segment never adopts one that is already there: a segment left behind by an
    /// earlier owner is still mapped by its readers, who go on interpreting it, and adopting it
    /// would also let the `ftruncate` fallback shrink it underneath them. The old segment is
    /// replaced instead - its readers keep what they have, new openers get the fresh one.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_named_shm_recreate_is_fresh() {
        let path = CString::new(format!("/recreate-own-{}", std::process::id())).unwrap();
        let first = NamedShmHandle::create(path.clone(), 5).unwrap();
        let mut mapped = first.map().unwrap();
        _ = mapped.as_slice_mut().write(&[9, 8, 7, 6, 5]).unwrap();

        let (again, replaced) = NamedShmHandle::create_replacing(path.clone(), 5)
            .expect("a pre-existing segment of our own must be replaced, not refused");
        let again = again.map().unwrap();
        assert_eq!(
            &again.as_slice()[..5],
            &[0; 5],
            "the new segment must not share backing with the old one"
        );
        assert_eq!(
            &mapped.as_slice()[..5],
            &[9, 8, 7, 6, 5],
            "the old segment must be left as its readers knew it"
        );

        assert_eq!(replaced.len(), 1, "the replaced segment is handed back");
        let replaced = replaced.into_iter().next().unwrap().map().unwrap();
        assert_eq!(&replaced.as_slice()[..5], &[9, 8, 7, 6, 5]);

        let opened = NamedShmHandle::open(&path).unwrap().map().unwrap();
        assert_eq!(
            &opened.as_slice()[..5],
            &[0; 5],
            "the name refers to the new one"
        );
    }

    /// The owner that was replaced must not remove the name on its way out: by then the name
    /// refers to its successor.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_replaced_owner_does_not_unlink_its_successor() {
        let path = CString::new(format!("/late-owner-{}", std::process::id())).unwrap();
        let old = NamedShmHandle::create(path.clone(), 16)
            .unwrap()
            .map()
            .unwrap();
        let new = NamedShmHandle::create(path.clone(), 16)
            .unwrap()
            .map()
            .unwrap();

        drop(old);
        assert!(
            NamedShmHandle::open(&path).is_ok(),
            "dropping the replaced owner must leave its successor's name in place"
        );

        drop(new);
        assert!(
            NamedShmHandle::open(&path).is_err(),
            "while the current owner still removes it"
        );
    }

    /// An owner replaced from another process cannot learn so from its own process's
    /// bookkeeping; the segment's identity has to tell it - on macOS too, where POSIX shared
    /// memory reports no inode. Covers every format at once: stats, limiters, one-way streams.
    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore)]
    fn an_owner_replaced_from_another_process_leaves_the_name() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let path = CString::new(format!("/cross-owner-{}", std::process::id())).unwrap();
        let old = NamedShmHandle::create(path.clone(), 16).unwrap();
        let (mut parent, mut child) = UnixStream::pair().unwrap();
        // SAFETY: the child only uses this library and its socket, and leaves through _exit.
        match unsafe { libc::fork() } {
            -1 => panic!("fork failed"),
            0 => {
                let replacement = NamedShmHandle::create(path.clone(), 16).unwrap();
                let ok = child.write_all(b"R").is_ok()
                    && child.read_exact(&mut [0]).is_ok()
                    && NamedShmHandle::open(&path).is_ok();
                drop(replacement);
                unsafe { libc::_exit(if ok { 0 } else { 1 }) }
            }
            pid => {
                parent.read_exact(&mut [0]).unwrap();
                drop(old);
                parent.write_all(b"D").unwrap();
                let mut status = 0;
                assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
                assert_eq!(
                    libc::WEXITSTATUS(status),
                    0,
                    "the successor's name was removed"
                );
                assert!(
                    NamedShmHandle::open(&path).is_err(),
                    "while the successor still removed it on its way out"
                );
            }
        }
    }

    /// A reader's handle never removes anything, however it is dropped.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn an_opened_handle_does_not_unlink() {
        let path = CString::new(format!("/reader-drop-{}", std::process::id())).unwrap();
        let owner = NamedShmHandle::create(path.clone(), 16).unwrap();
        let reader = NamedShmHandle::open(&path).unwrap();
        drop(reader);
        assert!(NamedShmHandle::open(&path).is_ok());
        drop(owner);
    }
    /// Growing a segment must not move it. This is the property the whole arrangement exists
    /// for: a reservation is mapped once and growth commits backing store underneath it, so
    /// callers may hold references into the segment across a growth and need no lock to keep
    /// readers away from a resize.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn growing_a_segment_does_not_move_it() {
        let shm = ShmHandle::new(4096).unwrap();
        let mut mapped = shm.map().unwrap();
        _ = mapped.as_slice_mut().write(&[7, 8, 9]).unwrap();

        let base = mapped.as_slice().as_ptr();
        let len_before = mapped.as_slice().len();
        // Taken before the growth, read after it.
        let first = &mapped.as_slice()[0];

        assert!(mapped.ensure_space(1 << 20));

        assert!(mapped.as_slice().len() >= 1 << 20);
        assert!(mapped.as_slice().len() > len_before);
        assert_eq!(mapped.as_slice().as_ptr(), base, "the mapping moved");
        assert_eq!(
            *first, 7,
            "a reference taken before the growth must survive it"
        );
        assert_eq!(
            &mapped.as_slice()[..3],
            &[7, 8, 9],
            "contents must survive too"
        );
    }

    /// `ensure_space` is idempotent and order-free: `fallocate` allocates a range and never
    /// shrinks the file, `VirtualAlloc(MEM_COMMIT)` tolerates already-committed pages, and
    /// macOS only raises a length counter. Concurrent callers asking for different sizes must
    /// therefore all end up with at least what they asked for, with no lock between them.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn concurrent_growth_settles_on_the_largest_request() {
        const SIZES: [usize; 6] = [1 << 16, 1 << 20, 1 << 18, 1 << 20, 1 << 14, 1 << 19];

        let shm = ShmHandle::new(4096).unwrap();
        let mapped = shm.map().unwrap();
        let base = mapped.as_slice().as_ptr();

        std::thread::scope(|scope| {
            for size in SIZES {
                let mapped = &mapped;
                scope.spawn(move || {
                    assert!(mapped.ensure_space(size), "growing to {size} failed");
                    assert!(
                        mapped.as_slice().len() >= size,
                        "a grower must at least see its own request"
                    );
                });
            }
        });

        assert!(mapped.as_slice().len() >= 1 << 20);
        assert_eq!(mapped.as_slice().as_ptr(), base, "the mapping moved");
    }
}
