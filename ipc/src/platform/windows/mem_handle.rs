use std::{io, mem};
use std::ffi::CString;
use std::io::Error;
use std::mem::MaybeUninit;
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::ptr::{null, null_mut};
use winapi::shared::minwindef::{DWORD, LPVOID};
use winapi::um::handleapi::INVALID_HANDLE_VALUE;
use winapi::um::winnt::{HANDLE, LPCSTR, MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_READWRITE, SEC_RESERVE};
use winapi::um::memoryapi::{FILE_MAP_WRITE, MapViewOfFile, UnmapViewOfFile, VirtualAlloc, VirtualQuery};
use winapi::um::winbase::{CreateFileMappingA, OpenFileMappingA};
use crate::platform::{FileBackedHandle, MappedMem, MemoryHandle, NamedShmHandle, PlatformHandle, ShmHandle, ShmPath};

const MAPPING_MAX_SIZE: usize = 100_000_000; // 100 MB ought to be enough for everybody?
const NOT_COMMITTED: usize = 1 << (usize::BITS - 1);

pub(crate) fn mmap_handle<T: FileBackedHandle>(mut handle: T) -> io::Result<MappedMem<T>> {
    let shm = handle.get_shm_mut();
    let ptr = unsafe { MapViewOfFile(
        shm.handle.as_raw_handle() as HANDLE,
        FILE_MAP_WRITE,
        0,
        0,
        MAPPING_MAX_SIZE,
    ) };
    if ptr.is_null() {
        return Err(Error::last_os_error());
    }
    if shm.size & NOT_COMMITTED != 0 {
        shm.size &= !NOT_COMMITTED;
        if shm.size == 0 {
            // We don't know the size of a freshly opened object yet. Query it.
            shm.size = unsafe {
                let mut info = MaybeUninit::<MEMORY_BASIC_INFORMATION>::uninit();
                VirtualQuery(ptr, info.as_mut_ptr(), mem::size_of::<MEMORY_BASIC_INFORMATION>());
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
            // Hence we resort to reserving space in the virtual mapping, which can be committed on demand
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

impl ShmHandle {
    pub fn new(size: usize) -> anyhow::Result<ShmHandle> {
        Ok(ShmHandle {
            handle: unsafe { PlatformHandle::from_raw_handle(alloc_shm(null())?) },
            size: size | NOT_COMMITTED
        })
    }
}

impl NamedShmHandle {
    fn format_name(path: String) -> String {
        format!("Global\\{}", &path[1..]) // strip leading slash
    }

    pub fn create(path: String, size: usize) -> io::Result<NamedShmHandle> {
        let name = Self::format_name(path);
        Self::new(alloc_shm(name.as_ptr() as LPCSTR)?, name, size | NOT_COMMITTED)
    }

    pub fn open(path: String) -> io::Result<NamedShmHandle> {
        let name = Self::format_name(path);
        let handle = unsafe { OpenFileMappingA(FILE_MAP_WRITE, 0, name.as_ptr() as LPCSTR) };
        // We need to map the handle to query
        Self::new(handle as RawHandle, name, NOT_COMMITTED)
    }

    fn new(handle: RawHandle, path: String, size: usize) -> io::Result<NamedShmHandle> {
        Ok(NamedShmHandle {
            inner: ShmHandle {
                handle: unsafe { PlatformHandle::from_raw_handle(handle) },
                size,
            },
            path: Some(ShmPath { name: CString::new(path).unwrap() }),
        })
    }
}

impl<T: FileBackedHandle + From<MappedMem<T>>> MappedMem<T> {
    pub fn ensure_space(mut self, expected_size: usize) -> MappedMem<T> {
        let current_size = self.mem.get_shm().size;
        if expected_size <= current_size {
            return self;
        }
        if expected_size > MAPPING_MAX_SIZE {
            panic!("Tried to allocate {} bytes for shared mapping (limit: {} bytes)", expected_size, MAPPING_MAX_SIZE);
        }

        unsafe {
            self.mem.set_mapping_size(expected_size).unwrap();
        }
        let new_size = self.mem.get_shm().size;
        unsafe { VirtualAlloc((self.ptr as usize + current_size) as LPVOID, new_size - current_size, MEM_COMMIT, PAGE_READWRITE) };
        self
    }
}
