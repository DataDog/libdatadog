// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use datadog_ipc::platform::{FileBackedHandle, MappedMem, NamedShmHandle, ShmHandle};
use ddcommon::Endpoint;
use std::hash::{Hash, Hasher};
use std::io;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use zwohash::ZwoHasher;

pub struct AgentRemoteConfigWriter<T>
where
    T: FileBackedHandle<T> + From<MappedMem<T>>,
{
    handle: Mutex<Option<MappedMem<T>>>,
}

pub struct AgentRemoteConfigReader<T>
where
    T: FileBackedHandle<T> + From<MappedMem<T>>,
{
    handle: Option<MappedMem<T>>,
    endpoint: Option<Endpoint>,
    current_config: Option<Vec<u8>>,
}

#[repr(C)]
#[derive(Debug)]
struct RawMetaData {
    generation: u64,
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

impl From<&[u8]> for &RawData {
    fn from(value: &[u8]) -> Self {
        // Safety: MappedMem is supposed to be big enough
        unsafe { &*(value as *const [u8] as *const RawData) }
    }
}

fn path_for_endpoint(endpoint: &Endpoint) -> String {
    let mut hasher = ZwoHasher::default();
    endpoint.url.authority().unwrap().hash(&mut hasher);
    format!("/libdatadog-agent-config-{}", hasher.finish())
}

pub fn create_anon_pair() -> anyhow::Result<(AgentRemoteConfigWriter<ShmHandle>, ShmHandle)> {
    let handle = ShmHandle::new(0x1000)?;
    Ok((
        AgentRemoteConfigWriter {
            handle: Mutex::new(Some(handle.clone().map()?)),
        },
        handle,
    ))
}

pub fn new_reader(endpoint: &Endpoint) -> AgentRemoteConfigReader<NamedShmHandle> {
    AgentRemoteConfigReader {
        handle: open_named_shm(endpoint).ok(),
        endpoint: Some(endpoint.clone()),
        current_config: None,
    }
}

pub fn reader_from_shm(handle: ShmHandle) -> io::Result<AgentRemoteConfigReader<ShmHandle>> {
    Ok(AgentRemoteConfigReader {
        handle: Some(handle.map()?),
        endpoint: None,
        current_config: None,
    })
}

pub fn new_writer(endpoint: &Endpoint) -> io::Result<AgentRemoteConfigWriter<NamedShmHandle>> {
    Ok(AgentRemoteConfigWriter {
        handle: Mutex::new(Some(
            NamedShmHandle::create(path_for_endpoint(endpoint), 0x1000)?.map()?,
        )),
    })
}

pub trait ReaderOpener<T>
where
    T: FileBackedHandle<T>,
{
    fn open(endpoint: &Endpoint) -> Option<MappedMem<T>>;
}

fn open_named_shm(endpoint: &Endpoint) -> io::Result<MappedMem<NamedShmHandle>> {
    NamedShmHandle::open(path_for_endpoint(endpoint))?.map()
}

impl ReaderOpener<NamedShmHandle> for AgentRemoteConfigReader<NamedShmHandle> {
    fn open(endpoint: &Endpoint) -> Option<MappedMem<NamedShmHandle>> {
        open_named_shm(endpoint).ok()
    }
}

impl ReaderOpener<ShmHandle> for AgentRemoteConfigReader<ShmHandle> {
    fn open(_: &Endpoint) -> Option<MappedMem<ShmHandle>> {
        None
    }
}

impl<T: FileBackedHandle<T> + From<MappedMem<T>>> AgentRemoteConfigReader<T>
where
    AgentRemoteConfigReader<T>: ReaderOpener<T>,
{
    // bool is true when it changed
    pub fn read<'a>(&'a mut self) -> (bool, &[u8]) {
        if let Some(ref handle) = self.handle {
            let source_data: &RawData = handle.as_slice().into();
            let new_generation = source_data.meta.generation;

            let fetch_data = |reader: &'a mut AgentRemoteConfigReader<T>| {
                let size = std::mem::size_of::<RawMetaData>() + source_data.meta.size;

                let handle = reader.handle.take().unwrap().ensure_space(size);
                reader.handle.replace(handle);
                let handle = reader.handle.as_ref().unwrap();

                let mut new_mem = Vec::with_capacity(size);
                new_mem.extend_from_slice(&handle.as_slice()[0..size]);

                // refetch, might have been resized
                let new_data: &RawData = handle.as_slice().into();
                let copied_data: &RawData = new_mem.as_slice().into();

                // Ensure the next write hasn't started yet *and* the data is from the expected generation
                if !new_data.meta.writing.load(Ordering::SeqCst)
                    && new_generation == copied_data.meta.generation
                {
                    reader.current_config.replace(new_mem);
                    return Some((true, new_data.as_slice()));
                }
                None
            };

            if let Some(cur_mem) = &self.current_config {
                let cur_data: &RawData = cur_mem.as_slice().into();
                // Ensure nothing is copied during a write
                if !source_data.meta.writing.load(Ordering::SeqCst)
                    && new_generation > cur_data.meta.generation
                {
                    if let Some(success) = fetch_data(self) {
                        return success;
                    }
                }

                return (false, cur_data.as_slice());
            } else if !source_data.meta.writing.load(Ordering::SeqCst) {
                if let Some(success) = fetch_data(self) {
                    return success;
                }
            }
        } else if let Some(ref endpoint) = self.endpoint {
            if let Some(handle) = Self::open(endpoint) {
                self.handle.replace(handle);
                return self.read();
            }
        }

        (false, "".as_bytes())
    }

    pub fn clear_reader(&mut self) {
        self.handle.take();
    }
}

impl<T: FileBackedHandle<T> + From<MappedMem<T>>> AgentRemoteConfigWriter<T> {
    pub fn write(&self, contents: &[u8]) {
        let mut handle = self.handle.lock().unwrap();
        let mut mapped = handle.take().unwrap();

        mapped = mapped.ensure_space(std::mem::size_of::<RawMetaData>() + contents.len());

        // Safety: ShmHandle is always big enough
        // Actually &mut mapped.as_slice_mut() as RawData seems safe, but unsized locals are unstable
        let data = unsafe { &mut *(mapped.as_slice_mut() as *mut [u8] as *mut RawData) };
        data.meta.writing.store(true, Ordering::SeqCst);
        data.meta.size = contents.len();

        _ = data.as_slice_mut().write(contents);

        data.meta.generation += 1;
        data.meta.writing.store(false, Ordering::SeqCst);

        handle.replace(mapped);
    }
}
