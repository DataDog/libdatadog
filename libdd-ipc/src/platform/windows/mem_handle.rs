// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::platform::mem_handle::page_aligned_size;
use crate::platform::{
    FileBackedHandle, MappedMem, MemoryHandle, NamedShmHandle, PlatformHandle, ShmHandle, ShmPath,
};
use std::ffi::{CStr, CString};
use std::io::Error;
use std::mem::MaybeUninit;
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::ptr::{null_mut, NonNull};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::{io, mem};
use winapi::shared::minwindef::{DWORD, LPVOID};
use winapi::um::handleapi::INVALID_HANDLE_VALUE;
use winapi::um::memoryapi::{
    MapViewOfFile, UnmapViewOfFile, VirtualAlloc, VirtualQuery, FILE_MAP_WRITE,
};
use winapi::um::winbase::{CreateFileMappingA, OpenFileMappingA};
use winapi::um::winnt::{
    HANDLE, LPCSTR, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_READWRITE, SEC_RESERVE,
};

const MAPPING_MAX_SIZE: usize = 100_000_000; // 100 MB ought to be enough for everybody?
const NOT_COMMITTED: usize = 1 << (usize::BITS - 1);

pub(crate) fn mmap_handle<T: FileBackedHandle>(mut handle: T) -> io::Result<MappedMem<T>> {
    let shm = handle.get_shm_mut();
    // The whole reservation in one view. The section was created with `SEC_RESERVE`, so this
    // costs address space and nothing else until pages are committed - and the view never has
    // to be replaced to make room, so every address in it is stable.
    let raw_ptr = unsafe {
        MapViewOfFile(
            shm.handle.as_raw_handle() as HANDLE,
            FILE_MAP_WRITE,
            0,
            0,
            MAPPING_MAX_SIZE,
        )
    };
    let Some(ptr) = NonNull::new(raw_ptr) else {
        return Err(Error::last_os_error());
    };
    if shm.size & NOT_COMMITTED != 0 {
        shm.size &= !NOT_COMMITTED;
        if shm.size == 0 {
            // We don't know the size of a freshly opened object yet. Query it: on a reserved
            // section the committed run at the base is exactly what its creator committed.
            shm.size = unsafe {
                let mut info = MaybeUninit::<MEMORY_BASIC_INFORMATION>::uninit();
                if VirtualQuery(
                    ptr.as_ptr().cast_const(),
                    info.as_mut_ptr(),
                    mem::size_of::<MEMORY_BASIC_INFORMATION>(),
                ) == 0
                {
                    return Err(Error::last_os_error());
                }
                info.assume_init().RegionSize
            };
        } else {
            unsafe { VirtualAlloc(ptr.as_ptr(), shm.size, MEM_COMMIT, PAGE_READWRITE) };
        }
    }
    let usable = shm.size.min(MAPPING_MAX_SIZE);
    Ok(MappedMem {
        ptr,
        mapped_len: MAPPING_MAX_SIZE,
        usable: AtomicUsize::new(usable),
        mem: handle,
    })
}

pub(crate) fn munmap_handle<T: MemoryHandle>(mapped: &mut MappedMem<T>) {
    unsafe {
        UnmapViewOfFile(mapped.ptr.as_ptr().cast_const());
    }
}

fn alloc_shm(name: LPCSTR) -> io::Result<RawHandle> {
    let handle = unsafe {
        CreateFileMappingA(
            INVALID_HANDLE_VALUE,
            null_mut(),
            // Windows does not allow for resizing file mappings (unlinke linux with ftruncate)
            // Hence we resort to reserving space in the virtual mapping, which can be committed on
            // demand
            PAGE_READWRITE | SEC_RESERVE,
            0,
            MAPPING_MAX_SIZE as DWORD,
            name,
        ) as RawHandle
    };
    if handle == 0 as RawHandle {
        return Err(Error::last_os_error());
    }
    Ok(handle)
}

static ANON_HANDLE_COUNTER: AtomicU32 = AtomicU32::new(0);

impl ShmHandle {
    pub fn new(size: usize) -> anyhow::Result<ShmHandle> {
        Self::new_named(size, "shm-handle")
    }

    pub fn new_named(size: usize, name: &str) -> anyhow::Result<ShmHandle> {
        // If one uses null_mut() for the name, DuplicateHandle will emit a very
        // confusing "The system cannot find the file specified. (os error 2)".
        // It seems like DuplicateHandle requires a name to re-open the FileMapping
        // within another process. Oh well. Let's generate an unique one.
        #[allow(clippy::unwrap_used)]
        let name = CString::new(format!(
            "libdatadog-anon-{name}-{}-{}",
            unsafe { libc::getpid() },
            ANON_HANDLE_COUNTER.fetch_add(1, Ordering::SeqCst)
        ))
        .unwrap();
        Ok(ShmHandle {
            handle: unsafe { PlatformHandle::from_raw_handle(alloc_shm(name.as_ptr() as LPCSTR)?) },
            size: size | NOT_COMMITTED,
        })
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

    pub fn create(path: CString, size: usize) -> io::Result<NamedShmHandle> {
        let name = Self::format_name(&path);
        Self::new(
            alloc_shm(name.as_ptr() as LPCSTR)?,
            path,
            size | NOT_COMMITTED,
        )
    }

    pub fn open(path: &CStr) -> io::Result<NamedShmHandle> {
        let name = Self::format_name(path);
        let handle = unsafe { OpenFileMappingA(FILE_MAP_WRITE, 0, name.as_ptr() as LPCSTR) };
        if handle.is_null() {
            return Err(Error::last_os_error());
        }
        // We need to map the handle to query its size, hence starting out with NOT_COMMITTED
        Self::new(handle as RawHandle, path.to_owned(), NOT_COMMITTED)
    }

    fn new(handle: RawHandle, name: CString, size: usize) -> io::Result<NamedShmHandle> {
        Ok(NamedShmHandle {
            inner: ShmHandle {
                handle: unsafe { PlatformHandle::from_raw_handle(handle) },
                size,
            },
            path: Some(Box::new(ShmPath { name })).into(),
        })
    }
}

impl<T: FileBackedHandle> MappedMem<T> {
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

        if unsafe {
            VirtualAlloc(
                self.ptr.as_ptr() as LPVOID,
                expected_size,
                MEM_COMMIT,
                PAGE_READWRITE,
            )
        }
        .is_null()
        {
            return false;
        }
        self.usable.fetch_max(expected_size, Ordering::AcqRel);
        true
    }
}
