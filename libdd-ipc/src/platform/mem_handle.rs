// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::handles::{HandlesTransport, TransferHandles};
use crate::platform::{mmap_handle, munmap_handle, OwnedFileHandle, PlatformHandle};
use crate::AtomicOption;
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
    pub(crate) ptr: NonNull<winapi::ctypes::c_void>,
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
}

pub struct NamedShmHandle {
    pub(crate) inner: ShmHandle,
    pub(crate) path: AtomicOption<Box<ShmPath>>,
}

impl NamedShmHandle {
    /// # Safety
    /// Must not be called concurrently with `unlink()`.
    pub unsafe fn get_path(&self) -> &[u8] {
        match self.path.as_option() {
            Some(shm_path) => shm_path.name.to_bytes(),
            None => b"",
        }
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
    /// Unlink the backing SHM file from the filesystem so new openers get `ENOENT`.
    /// Existing mappings remain valid.  On Windows the mapping is managed by the OS
    /// via handle reference counts and there is no filesystem entry to remove.
    #[cfg(unix)]
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
        self.mem.get_path()
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

    /// Creating a segment is exclusive (`O_EXCL`) so that one planted by another user is
    /// refused rather than adopted - but shared memory outlives the process that made it, so a
    /// segment left behind by an earlier sidecar of our own must still be picked up, not
    /// rejected. Without this, restarting a sidecar would fail on every one of its own segments.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_named_shm_recreate_adopts_our_own() {
        let path = CString::new("/recreate-own").unwrap();
        let first = NamedShmHandle::create(path.clone(), 5).unwrap();
        let mut mapped = first.map().unwrap();
        _ = mapped.as_slice_mut().write(&[9, 8, 7, 6, 5]).unwrap();

        let again = NamedShmHandle::create(path.clone(), 5)
            .expect("a pre-existing segment of our own must be adopted");
        let again = again.map().unwrap();
        assert_eq!(&again.as_slice()[..5], &[9, 8, 7, 6, 5]);
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
