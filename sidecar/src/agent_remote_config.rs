// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::one_way_shared_memory::{
    open_named_shm, OneWayShmReader, OneWayShmWriter, ReaderOpener,
};
use crate::primary_sidecar_identifier;
use datadog_ipc::platform::{FileBackedHandle, MappedMem, NamedShmHandle, ShmHandle};
use ddcommon::Endpoint;
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use std::io;
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
    CString::new(format!(
        "/ddcfg-{}-{}",
        primary_sidecar_identifier(),
        hasher.finish()
    ))
    .unwrap()
}

pub fn create_anon_pair() -> anyhow::Result<(AgentRemoteConfigWriter<ShmHandle>, ShmHandle)> {
    let (writer, handle) = crate::one_way_shared_memory::create_anon_pair()?;
    Ok((AgentRemoteConfigWriter(writer), handle))
}

pub fn new_reader(endpoint: &Endpoint) -> AgentRemoteConfigReader<NamedShmHandle> {
    AgentRemoteConfigReader(OneWayShmReader::new(
        open_named_shm(&path_for_endpoint(endpoint)).ok(),
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
            .and_then(|endpoint| open_named_shm(&path_for_endpoint(&endpoint.0)).ok())
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
