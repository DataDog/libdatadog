// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::one_way_shared_memory::{
    open_named_shm, OneWayShmReader, OneWayShmWriter, ReaderOpener,
};
use crate::primary_sidecar_identifier;
use datadog_ipc::platform::{FileBackedHandle, MappedMem, NamedShmHandle, ShmHandle};
use ddcommon_net1::Endpoint;
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use std::io;
use tracing::{trace, warn};
use zwohash::ZwoHasher;

pub struct AgentRemoteConfigEndpoint(Endpoint);

pub struct AgentRemoteConfigWriter<T: FileBackedHandle + From<MappedMem<T>>>(OneWayShmWriter<T>);
pub struct AgentRemoteConfigReader<T: FileBackedHandle + From<MappedMem<T>>>(
    OneWayShmReader<T, Option<AgentRemoteConfigEndpoint>>,
);

fn path_for_endpoint(endpoint: &Endpoint) -> CString {
    // We need a stable hash so that the outcome is independent of the process
    let mut hasher = ZwoHasher::default();
    endpoint.url.authority().unwrap().hash(&mut hasher);
    endpoint.test_token.hash(&mut hasher);
    CString::new(format!(
        "/ddcfg-{}-{}", // short enough because 31 character macos limitation
        primary_sidecar_identifier(),
        hasher.finish()
    ))
    .unwrap()
}

pub fn create_anon_pair() -> anyhow::Result<(AgentRemoteConfigWriter<ShmHandle>, ShmHandle)> {
    let (writer, handle) = crate::one_way_shared_memory::create_anon_pair()?;
    Ok((AgentRemoteConfigWriter(writer), handle))
}

fn try_open_shm(endpoint: &Endpoint) -> Option<MappedMem<NamedShmHandle>> {
    let path = &path_for_endpoint(endpoint);
    match open_named_shm(path) {
        Ok(mapped) => {
            trace!("Opened and loaded {path:?} for agent remote config.");
            Some(mapped)
        }
        Err(e) => {
            if e.raw_os_error().unwrap_or(0) != libc::ENOENT {
                warn!("Tried to open path {path:?} for agent remote config, but failed: {e:?}");
            } else {
                trace!("Found {path:?} is not available yet for agent remote config");
            }
            None
        }
    }
}

pub fn new_reader(endpoint: &Endpoint) -> AgentRemoteConfigReader<NamedShmHandle> {
    AgentRemoteConfigReader(OneWayShmReader::new(
        try_open_shm(endpoint),
        Some(AgentRemoteConfigEndpoint(endpoint.clone())),
    ))
}

pub fn reader_from_shm(handle: ShmHandle) -> io::Result<AgentRemoteConfigReader<ShmHandle>> {
    Ok(AgentRemoteConfigReader(OneWayShmReader::new(
        Some(handle.map()?),
        None,
    )))
}

pub fn new_writer(endpoint: &Endpoint) -> io::Result<AgentRemoteConfigWriter<NamedShmHandle>> {
    Ok(AgentRemoteConfigWriter(
        OneWayShmWriter::<NamedShmHandle>::new(path_for_endpoint(endpoint))?,
    ))
}

impl ReaderOpener<ShmHandle> for OneWayShmReader<ShmHandle, Option<AgentRemoteConfigEndpoint>> {}

impl ReaderOpener<NamedShmHandle>
    for OneWayShmReader<NamedShmHandle, Option<AgentRemoteConfigEndpoint>>
{
    fn open(&self) -> Option<MappedMem<NamedShmHandle>> {
        self.extra
            .as_ref()
            .and_then(|endpoint| try_open_shm(&endpoint.0))
    }
}

impl<T: FileBackedHandle + From<MappedMem<T>>> AgentRemoteConfigReader<T>
where
    OneWayShmReader<T, Option<AgentRemoteConfigEndpoint>>: ReaderOpener<T>,
{
    pub fn read(&mut self) -> (bool, &[u8]) {
        self.0.read()
    }
}

impl<T: FileBackedHandle + From<MappedMem<T>>> AgentRemoteConfigWriter<T> {
    pub fn write(&self, contents: &[u8]) {
        self.0.write(contents)
    }

    pub fn size(&self) -> usize {
        self.0.size()
    }
}
