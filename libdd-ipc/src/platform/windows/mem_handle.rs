// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::platform::mem_handle::page_aligned_size;
use crate::platform::{
    FileBackedHandle, MappedMem, MemoryHandle, NamedShmHandle, OwnedFileHandle, PlatformHandle,
    ShmHandle, ShmPath,
};
use std::ffi::{CStr, CString};
use std::io::Error;
use std::mem::MaybeUninit;
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::ptr::{NonNull, null_mut};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::{io, mem};
use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingA, FILE_MAP_WRITE, MEM_COMMIT, MEMORY_BASIC_INFORMATION,
    MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingA, PAGE_READWRITE, SEC_RESERVE,
    UnmapViewOfFile, VirtualAlloc, VirtualQuery,
};
use windows_sys::core::PCSTR;

const MAPPING_MAX_SIZE: usize = 100_000_000; // 100 MB ought to be enough for everybody?
const NOT_COMMITTED: usize = 1 << (usize::BITS - 1);

pub(crate) fn mmap_handle<T: FileBackedHandle>(mut handle: T) -> io::Result<MappedMem<T>> {
    let shm = handle.get_shm_mut();
    // Reserve one fixed view; committing pages later never moves existing data.
    let raw_ptr = unsafe {
        MapViewOfFile(
            shm.handle.as_raw_handle() as HANDLE,
            FILE_MAP_WRITE,
            0,
            0,
            MAPPING_MAX_SIZE,
        )
    };
    let Some(ptr) = NonNull::new(raw_ptr.Value) else {
        return Err(Error::last_os_error());
    };
    if shm.size & NOT_COMMITTED != 0 {
        shm.size &= !NOT_COMMITTED;
        let size = if shm.size == 0 {
            // Only committed pages are safe to touch. Retry if the creator has not committed yet.
            Some(committed_prefix(ptr))
                .filter(|&size| size > 0)
                .ok_or_else(|| io::Error::other("shared memory mapping size not yet committed"))
        } else if commit(ptr, shm.size) {
            Ok(shm.size)
        } else {
            Err(Error::last_os_error())
        };
        let size = match size {
            Ok(size) => size,
            Err(e) => {
                unsafe {
                    UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                        Value: ptr.as_ptr(),
                    })
                };
                return Err(e);
            }
        };
        shm.size = size;
    }
    let usable = shm.size.min(MAPPING_MAX_SIZE);
    Ok(MappedMem {
        ptr,
        mapped_len: MAPPING_MAX_SIZE,
        usable: AtomicUsize::new(usable),
        mem: handle,
    })
}

/// The length of the committed run at `ptr`, 0 if its first page is not committed.
fn committed_prefix(ptr: NonNull<std::ffi::c_void>) -> usize {
    unsafe {
        let mut info = MaybeUninit::<MEMORY_BASIC_INFORMATION>::uninit();
        if VirtualQuery(
            ptr.as_ptr().cast_const(),
            info.as_mut_ptr(),
            mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        ) == 0
        {
            return 0;
        }
        let info = info.assume_init();
        // An uncommitted base reports reserved space, which is not safe to touch.
        if info.State == MEM_COMMIT {
            info.RegionSize
        } else {
            0
        }
    }
}

/// Commit the first `size` bytes of a view of a reserved section, for every view of it.
fn commit(ptr: NonNull<std::ffi::c_void>, size: usize) -> bool {
    !unsafe { VirtualAlloc(ptr.as_ptr(), size, MEM_COMMIT, PAGE_READWRITE) }.is_null()
}

pub(crate) fn munmap_handle<T: MemoryHandle>(mapped: &mut MappedMem<T>) {
    unsafe {
        UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
            Value: mapped.ptr.as_ptr(),
        });
    }
}

type Section = PlatformHandle<OwnedFileHandle>;

fn section(handle: HANDLE) -> io::Result<Section> {
    if handle == 0 {
        return Err(Error::last_os_error());
    }
    // SAFETY: a handle just returned to us, owned by nobody else.
    Ok(unsafe { PlatformHandle::from_raw_handle(handle as RawHandle) })
}

fn open_section(name: &CStr) -> io::Result<Section> {
    section(unsafe { OpenFileMappingA(FILE_MAP_WRITE, 0, name.as_ptr() as PCSTR) })
}

/// Create or open a section, returning whether it already existed.
/// Windows reports collisions after opening; callers must discard an existing section.
fn alloc_shm(name: PCSTR) -> io::Result<(Section, bool)> {
    let maximum_size = u32::try_from(MAPPING_MAX_SIZE)
        .map_err(|error| Error::new(io::ErrorKind::InvalidInput, error))?;
    let handle = unsafe {
        CreateFileMappingA(
            INVALID_HANDLE_VALUE,
            null_mut(),
            // Windows sections cannot grow, so reserve space and commit pages on demand.
            PAGE_READWRITE | SEC_RESERVE,
            0,
            maximum_size,
            name,
        )
    };
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    Ok((section(handle)?, existed))
}

fn next_section_id() -> u64 {
    // Seed from monotonic time so reused PIDs do not repeat names still held by a sidecar.
    // Zero is reserved for an empty named-section index.
    static NEXT: LazyLock<AtomicU64> =
        LazyLock::new(|| AtomicU64::new(libdd_common::rate_limiter::now().max(1)));
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl ShmHandle {
    pub fn new(size: usize) -> anyhow::Result<ShmHandle> {
        Self::new_named(size, "shm-handle")
    }

    pub fn new_named(size: usize, name: &str) -> anyhow::Result<ShmHandle> {
        // Handle transfer requires a name. Retry collisions without modifying existing sections.
        const ATTEMPTS: usize = 16;
        for _ in 0..ATTEMPTS {
            #[allow(clippy::unwrap_used)]
            let name = CString::new(format!(
                "libdatadog-anon-{name}-{}-{}",
                std::process::id(),
                next_section_id()
            ))
            .unwrap();
            let (handle, existed) = alloc_shm(name.as_ptr() as PCSTR)?;
            if existed {
                continue;
            }
            return Ok(ShmHandle {
                handle,
                size: size | NOT_COMMITTED,
            });
        }
        anyhow::bail!("no unused name for an anonymous shared memory section")
    }

    /// Refresh the size of the shared memory segment
    /// No-op on Windows: the view is a fixed size and carries its committed length in itself,
    /// so a wire-supplied size cannot make a mapping exceed its backing. Kept so callers
    /// guarding a peer-supplied handle need no `cfg`.
    pub(crate) fn limit_size_to_backing(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    pub fn adjust_to_file_size(&mut self) -> std::io::Result<()> {
        self.size = NOT_COMMITTED;
        Ok(())
    }
}

/// A logical name points to a section with a unique suffix: Windows keeps a section's name
/// until its last handle closes. Reusing this small index lets readers find fresh sections.
/// Readers keep the index open so a new owner can find and retire its predecessor after a crash.
pub(crate) struct ShmIndex {
    view: NonNull<std::ffi::c_void>,
    _handle: Section,
}

// SAFETY: a handle and a view of shared memory, used only through the atomic in it.
unsafe impl Send for ShmIndex {}
unsafe impl Sync for ShmIndex {}

const INDEX_SIZE: usize = 0x1000;

impl ShmIndex {
    fn create_or_open(name: &CStr) -> io::Result<ShmIndex> {
        let handle = unsafe {
            CreateFileMappingA(
                INVALID_HANDLE_VALUE,
                null_mut(),
                PAGE_READWRITE,
                0,
                INDEX_SIZE as u32,
                name.as_ptr() as PCSTR,
            )
        };
        Self::map(section(handle)?)
    }

    fn open(name: &CStr) -> io::Result<ShmIndex> {
        Self::map(open_section(name)?)
    }

    fn map(handle: Section) -> io::Result<ShmIndex> {
        let view = unsafe {
            MapViewOfFile(
                handle.as_raw_handle() as HANDLE,
                FILE_MAP_WRITE,
                0,
                0,
                INDEX_SIZE,
            )
        };
        match NonNull::new(view.Value) {
            Some(view) => Ok(ShmIndex {
                view,
                _handle: handle,
            }),
            None => Err(Error::last_os_error()),
        }
    }

    /// The suffix of the section in service, or 0 for none.
    fn current(&self) -> &AtomicU64 {
        // SAFETY: the view is a page long and page-aligned, and stays mapped as long as `self`.
        unsafe { &*self.view.as_ptr().cast() }
    }

    /// Clear the index only if it still points at our section.
    pub(crate) fn release(&self, suffix: u64) {
        _ = self
            .current()
            .compare_exchange(suffix, 0, Ordering::AcqRel, Ordering::Relaxed);
    }
}

impl Drop for ShmIndex {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.view.as_ptr(),
            })
        };
    }
}

impl Drop for ShmPath {
    fn drop(&mut self) {
        if let Some(suffix) = self.owned {
            self.index.release(suffix);
        }
    }
}

/// Commit the first `size` bytes of a freshly created reserved section, through a transient view.
fn commit_section(handle: &Section, size: usize) -> io::Result<()> {
    let view = unsafe {
        MapViewOfFile(
            handle.as_raw_handle() as HANDLE,
            FILE_MAP_WRITE,
            0,
            0,
            MAPPING_MAX_SIZE,
        )
    };
    let Some(view) = NonNull::new(view.Value) else {
        return Err(Error::last_os_error());
    };
    let committed = commit(view, size.max(1));
    let error = Error::last_os_error();
    unsafe {
        UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
            Value: view.as_ptr(),
        })
    };
    if committed { Ok(()) } else { Err(error) }
}

impl NamedShmHandle {
    fn format_name(path: &CStr) -> CString {
        // Global\ namespace is reserved for Session ID 0.
        // We cannot rely on our PHP process having permissions to have access to Session 0.
        // This requires us to have one sidecar per Session ID. That's good enough though.
        #[allow(clippy::unwrap_used)]
        CString::new(format!(
            "Local\\{}",
            String::from_utf8_lossy(&path.to_bytes()[1..])
        ))
        .unwrap() // strip leading slash
    }

    fn physical_name(name: &CStr, suffix: u64) -> CString {
        #[allow(clippy::unwrap_used)] // appending hex digits adds no NUL
        CString::new(format!("{}.{suffix:016x}", name.to_string_lossy())).unwrap()
    }

    /// As [`Self::create`], also returning a handle to the segment that was replaced, if any.
    pub fn create_replacing(
        path: CString,
        size: usize,
    ) -> io::Result<(NamedShmHandle, Vec<NamedShmHandle>)> {
        let name = Self::format_name(&path);
        let index = ShmIndex::create_or_open(&name)?;

        const ATTEMPTS: usize = 16;
        let mut created = None;
        for _ in 0..ATTEMPTS {
            let suffix = next_section_id();
            let physical = Self::physical_name(&name, suffix);
            let (handle, existed) = alloc_shm(physical.as_ptr() as PCSTR)?;
            if existed {
                continue;
            }
            created = Some((handle, suffix));
            break;
        }
        let Some((handle, suffix)) = created else {
            return Err(Error::new(
                io::ErrorKind::AlreadyExists,
                "no unused name for a shared memory section",
            ));
        };

        // Commit before publishing so readers can safely access the new section.
        commit_section(&handle, size)?;
        let previous = index.current().swap(suffix, Ordering::AcqRel);
        let replaced = if previous == 0 {
            vec![]
        } else {
            // Gone if nobody holds it any more.
            open_section(&Self::physical_name(&name, previous))
                .map(|old| Self::new(old, None, NOT_COMMITTED))
                .into_iter()
                .collect()
        };

        let shm_path = ShmPath {
            name: path,
            index,
            owned: Some(suffix),
        };
        Ok((
            Self::new(handle, Some(shm_path), size | NOT_COMMITTED),
            replaced,
        ))
    }

    pub fn open(path: &CStr) -> io::Result<NamedShmHandle> {
        let name = Self::format_name(path);
        let index = ShmIndex::open(&name)?;
        let current = index.current().load(Ordering::Acquire);
        if current == 0 {
            return Err(Error::from_raw_os_error(ERROR_FILE_NOT_FOUND as i32));
        }
        let physical = Self::physical_name(&name, current);
        let handle = open_section(&physical)?;
        let shm_path = ShmPath {
            name: path.to_owned(),
            index,
            owned: None,
        };
        // We need to map the handle to query its size, hence starting out with NOT_COMMITTED
        Ok(Self::new(handle, Some(shm_path), NOT_COMMITTED))
    }

    fn new(handle: Section, path: Option<ShmPath>, size: usize) -> NamedShmHandle {
        NamedShmHandle {
            inner: ShmHandle { handle, size },
            path: path.map(Box::new).into(),
        }
    }
}

impl<T: FileBackedHandle> MappedMem<T> {
    /// Refresh the usable length from pages committed by any process, without allocating.
    pub fn refresh_size(&self) -> usize {
        let committed = committed_prefix(self.ptr);
        self.usable
            .fetch_max(committed.min(self.mapped_len), Ordering::AcqRel);
        self.get_size()
    }

    /// Commit `expected_size` bytes of the reserved view, leaving the view where it is, and
    /// report whether that many bytes are now usable.
    ///
    /// Committing a page that is already committed is explicitly allowed, so this commits the
    /// whole range from the base rather than tracking a delta: any number of callers may then
    /// do it concurrently, in any order, and the result is the same. Windows cannot resize a
    /// section at all, which is why the view covers the reservation from the start.
    ///
    /// `false` means the request exceeds the reservation, or the commit failed, and nothing
    /// may be written past what the view already has.
    #[must_use = "a segment that could not be grown is still too short to write to"]
    pub fn ensure_space(&self, expected_size: usize) -> bool {
        if expected_size <= self.get_size() {
            return true;
        }
        let expected_size = page_aligned_size(expected_size);
        if expected_size > self.mapped_len {
            return false;
        }

        if !commit(self.ptr, expected_size) {
            return false;
        }
        self.usable.fetch_max(expected_size, Ordering::AcqRel);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anonymous_sections_survive_pid_reuse() {
        let name = "previous-process";
        // A sidecar can still hold these sections after Windows reuses the creator's PID.
        let old: Vec<_> = (0..16)
            .map(|counter| {
                let name = CString::new(format!(
                    "libdatadog-anon-{name}-{}-{counter}",
                    std::process::id()
                ))
                .unwrap();
                let (handle, existed) = alloc_shm(name.as_ptr() as PCSTR).unwrap();
                assert!(!existed);
                let mut mapped = ShmHandle {
                    handle,
                    size: 4096 | NOT_COMMITTED,
                }
                .map()
                .unwrap();
                mapped.as_slice_mut()[0] = 42;
                mapped
            })
            .collect();

        let new = ShmHandle::new_named(4096, name).unwrap().map().unwrap();
        assert!(new.as_slice().iter().all(|&byte| byte == 0));
        assert!(old.iter().all(|mapped| mapped.as_slice()[0] == 42));
    }

    /// A section nothing was committed in yet has no readable pages: mapping it must fail
    /// rather than report its whole reservation as usable, which faults on first touch.
    #[test]
    fn an_uncommitted_section_is_not_mappable() {
        #[allow(clippy::unwrap_used)]
        let name = CString::new(format!("libdatadog-uncommitted-{}", std::process::id())).unwrap();
        let (_created, existed) = alloc_shm(name.as_ptr() as PCSTR).unwrap();
        assert!(!existed);
        let reader = NamedShmHandle::new(open_section(&name).unwrap(), None, NOT_COMMITTED);
        assert!(reader.map().is_err(), "reserved pages must not be exposed");
    }

    /// The name only points at a section once its pages are committed, so an opener racing the
    /// creator - before the creator even maps it - gets readable memory or nothing.
    #[test]
    fn an_open_before_the_creator_maps_reads_committed_memory() {
        #[allow(clippy::unwrap_used)]
        let path = CString::new(format!("/dd-uncommitted-{}", std::process::id())).unwrap();
        let owner = NamedShmHandle::create(path.clone(), 4096).unwrap();
        let reader = NamedShmHandle::open(&path).unwrap().map().unwrap();
        assert!(!reader.as_slice().is_empty());
        // Every byte handed out must be touchable.
        assert!(reader.as_slice().iter().all(|&b| b == 0));
        let _owner = owner.map().unwrap();
    }
}
