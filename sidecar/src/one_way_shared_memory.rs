// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_ipc::platform::{FileBackedHandle, MappedMem, NamedShmHandle, ShmHandle};
use std::ffi::{CStr, CString};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

pub struct OneWayShmWriter<T>
where
    T: FileBackedHandle + From<MappedMem<T>>,
{
    handle: Mutex<MappedMem<T>>,
}

pub struct OneWayShmReader<T, D>
where
    T: FileBackedHandle + From<MappedMem<T>>,
{
    handle: Option<MappedMem<T>>,
    current_data: Option<Vec<u64>>,
    pub extra: D,
}

#[repr(C)]
#[derive(Debug)]
struct RawMetaData {
    generation: AtomicU64,
    size: usize,
    writing: AtomicBool,
}

#[repr(C)]
#[derive(Debug)]
struct RawData {
    meta: RawMetaData,
    buf: [u8],
}

impl RawData {
    fn as_slice(&self) -> &[u8] {
        // Safety: size is expected to be truthful
        unsafe { std::slice::from_raw_parts(self.buf.as_ptr(), self.meta.size) }
    }

    fn as_slice_mut(&mut self) -> &mut [u8] {
        // Safety: size is expected to be truthful
        unsafe { std::slice::from_raw_parts_mut(self.buf.as_mut_ptr(), self.meta.size) }
    }
}

impl From<&[u64]> for &RawData {
    fn from(value: &[u64]) -> Self {
        // Safety: MappedMem is supposed to be big enough
        // Safety: u64 is aligned
        unsafe { &*(value as *const [u64] as *const RawData) }
    }
}

// Safety: Caller needs to ensure the u8 is 8 byte aligned
unsafe fn reinterpret_u8_as_u64_slice(slice: &[u8]) -> &[u64] {
    // Safety: given 8 byte alignment, it's guaranteed to be readable
    std::slice::from_raw_parts(slice.as_ptr() as *const u64, (slice.len() + 7) / 8)
}

pub fn create_anon_pair() -> anyhow::Result<(OneWayShmWriter<ShmHandle>, ShmHandle)> {
    let handle = ShmHandle::new(0x1000)?;
    Ok((
        OneWayShmWriter {
            handle: Mutex::new(handle.clone().map()?),
        },
        handle,
    ))
}

impl<T: FileBackedHandle + From<MappedMem<T>>, D> OneWayShmReader<T, D> {
    pub fn new(handle: Option<MappedMem<T>>, extra: D) -> OneWayShmReader<T, D> {
        OneWayShmReader {
            handle,
            current_data: None,
            extra,
        }
    }
}

impl<T: FileBackedHandle + From<MappedMem<T>>> OneWayShmWriter<T> {
    pub fn new(path: CString) -> io::Result<OneWayShmWriter<NamedShmHandle>> {
        Ok(OneWayShmWriter {
            handle: Mutex::new(NamedShmHandle::create(path, 0x1000)?.map()?),
        })
    }
}

pub trait ReaderOpener<T>
where
    T: FileBackedHandle,
{
    fn open(&self) -> Option<MappedMem<T>> {
        None
    }
}

pub fn open_named_shm(path: &CStr) -> io::Result<MappedMem<NamedShmHandle>> {
    NamedShmHandle::open(path)?.map()
}

fn skip_last_byte(slice: &[u8]) -> &[u8] {
    if slice.is_empty() {
        slice
    } else {
        &slice[..slice.len() - 1]
    }
}

impl<T: FileBackedHandle + From<MappedMem<T>>, D> OneWayShmReader<T, D>
where
    OneWayShmReader<T, D>: ReaderOpener<T>,
{
    // bool is true when it changed
    pub fn read<'a>(&'a mut self) -> (bool, &[u8]) {
        if let Some(ref handle) = self.handle {
            let source_data: &RawData =
                unsafe { reinterpret_u8_as_u64_slice(handle.as_slice()) }.into();
            let new_generation = source_data.meta.generation.load(Ordering::Acquire);

            let fetch_data = |reader: &'a mut OneWayShmReader<T, D>| {
                let size = std::mem::size_of::<RawMetaData>() + source_data.meta.size;

                let handle = reader.handle.as_mut().unwrap();
                handle.ensure_space(size);

                // aligned on 8 byte boundary, round up to closest 8 byte boundary
                let mut new_mem = Vec::<u64>::with_capacity((size + 7) / 8);
                new_mem.extend_from_slice(unsafe {
                    reinterpret_u8_as_u64_slice(&handle.as_slice()[0..size])
                });

                // refetch, might have been resized
                let source_data: &RawData =
                    unsafe { reinterpret_u8_as_u64_slice(handle.as_slice()) }.into();
                let copied_data: &RawData = new_mem.as_slice().into();

                // Ensure the next write hasn't started yet *and* the data is from the expected
                // generation
                if !source_data.meta.writing.load(Ordering::SeqCst)
                    && new_generation == source_data.meta.generation.load(Ordering::Acquire)
                {
                    reader.current_data.replace(new_mem);
                    return Some((true, skip_last_byte(copied_data.as_slice())));
                }
                None
            };

            if let Some(cur_mem) = &self.current_data {
                let cur_data: &RawData = cur_mem.as_slice().into();
                // Ensure nothing is copied during a write
                if !source_data.meta.writing.load(Ordering::SeqCst)
                    && new_generation > cur_data.meta.generation.load(Ordering::Acquire)
                {
                    if let Some(success) = fetch_data(self) {
                        return success;
                    }
                }

                return (false, skip_last_byte(cur_data.as_slice()));
            } else if !source_data.meta.writing.load(Ordering::SeqCst) {
                if let Some(success) = fetch_data(self) {
                    return success;
                }
            }
        } else if let Some(handle) = self.open() {
            self.handle.replace(handle);
            return self.read();
        }

        (false, "".as_bytes())
    }

    pub fn clear_reader(&mut self) {
        self.handle.take();
    }
}

impl<T: FileBackedHandle + From<MappedMem<T>>> OneWayShmWriter<T> {
    pub fn write(&self, contents: &[u8]) {
        let mut mapped = self.handle.lock().unwrap();

        let size = contents.len() + 1; // trailing zero byte, to keep some C code happy
        mapped.ensure_space(std::mem::size_of::<RawMetaData>() + size);

        // Safety: ShmHandle is always big enough
        // Actually &mut mapped.as_slice_mut() as RawData seems safe, but unsized locals are
        // unstable
        let data = unsafe { &mut *(mapped.as_slice_mut() as *mut [u8] as *mut RawData) };
        data.meta.writing.store(true, Ordering::SeqCst);
        data.meta.size = size;

        data.as_slice_mut()[0..contents.len()].copy_from_slice(contents);
        data.as_slice_mut()[contents.len()] = 0;

        data.meta.generation.fetch_add(1, Ordering::SeqCst);
        data.meta.writing.store(false, Ordering::SeqCst);
    }

    pub fn as_slice(&self) -> &[u8] {
        let mapped = self.handle.lock().unwrap();
        let data = unsafe { &*(mapped.as_slice() as *const [u8] as *const RawData) };
        if data.meta.size > 0 {
            let slice = data.as_slice();
            &slice[..slice.len() - 1] // ignore the trailing zero
        } else {
            b""
        }
    }

    pub fn size(&self) -> usize {
        self.handle.lock().unwrap().as_slice().len()
    }
}
