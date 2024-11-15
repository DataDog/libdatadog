// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::platform::{
    FileBackedHandle, MappedMem, MemoryHandle, NamedShmHandle, PlatformHandle, ShmHandle, ShmPath,
};
use std::ffi::{CStr, CString};
use std::io::Error;
use std::mem::MaybeUninit;
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicU32, Ordering};
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
    let ptr = unsafe {
        MapViewOfFile(
            shm.handle.as_raw_handle() as HANDLE,
            FILE_MAP_WRITE,
            0,
            0,
            MAPPING_MAX_SIZE,
        )
    };
    if ptr.is_null() {
        return Err(Error::last_os_error());
    }
    if shm.size & NOT_COMMITTED != 0 {
        shm.size &= !NOT_COMMITTED;
        if shm.size == 0 {
            // We don't know the size of a freshly opened object yet. Query it.
            shm.size = unsafe {
                let mut info = MaybeUninit::<MEMORY_BASIC_INFORMATION>::uninit();
                if VirtualQuery(
                    ptr,
                    info.as_mut_ptr(),
                    mem::size_of::<MEMORY_BASIC_INFORMATION>(),
                ) == 0
                {
                    return Err(Error::last_os_error());
                }
                info.assume_init().RegionSize
            };
        } else {
            unsafe { VirtualAlloc(ptr, shm.size, MEM_COMMIT, PAGE_READWRITE) };
        }
    }
    Ok(MappedMem { ptr, mem: handle })
}

pub(crate) fn munmap_handle<T: MemoryHandle>(mapped: &mut MappedMem<T>) {
    unsafe {
        UnmapViewOfFile(mapped.ptr.cast_const());
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
}

impl NamedShmHandle {
    fn format_name(path: &CStr) -> CString {
        // Global\ namespace is reserved for Session ID 0.
        // We cannot rely on our PHP process having permissions to have access to Session 0.
        // This requires us to have one sidecar per Session ID. That's good enough though.
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
            path: Some(ShmPath { name }),
        })
    }
}

impl<T: FileBackedHandle + From<MappedMem<T>>> MappedMem<T> {
    pub fn ensure_space(&mut self, expected_size: usize) {
        let current_size = self.mem.get_shm().size;
        if expected_size <= current_size {
            return;
        }
        if expected_size > MAPPING_MAX_SIZE {
            panic!(
                "Tried to allocate {} bytes for shared mapping (limit: {} bytes)",
                expected_size, MAPPING_MAX_SIZE
            );
        }

        unsafe {
            self.mem.set_mapping_size(expected_size).unwrap();
        }
        let new_size = self.mem.get_shm().size;
        unsafe {
            VirtualAlloc(
                (self.ptr as usize + current_size) as LPVOID,
                new_size - current_size,
                MEM_COMMIT,
                PAGE_READWRITE,
            )
        };
    }
}
