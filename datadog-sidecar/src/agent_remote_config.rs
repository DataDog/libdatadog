// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_common::Endpoint;
use libdd_ipc::one_way_shared_memory::{OneWayShmReader, OneWayShmWriter, open_named_shm};
use libdd_ipc::platform::{FileBackedHandle, MappedMem, NamedShmHandle, ShmHandle};
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use std::io;
use tracing::{trace, warn};
use zwohash::ZwoHasher;

pub struct AgentRemoteConfigEndpoint(Endpoint);

pub struct AgentRemoteConfigWriter<T: FileBackedHandle>(OneWayShmWriter<T>);
pub struct AgentRemoteConfigReader<T: FileBackedHandle>(
    OneWayShmReader<T, Option<AgentRemoteConfigEndpoint>>,
);

fn path_for_endpoint(endpoint: &Endpoint) -> CString {
    // We need a stable hash so that the outcome is independent of the process
    let mut hasher = ZwoHasher::default();

    #[allow(clippy::unwrap_used)]
    endpoint.url.authority().unwrap().hash(&mut hasher);
    endpoint.test_token.hash(&mut hasher);

    let mut path = format!("/ddcfg-{}-{}", crate::shm_namespace(), hasher.finish());
    if cfg!(unix) {
        path.truncate(31); // macOS limits shared memory names to 31 bytes.
    }
    #[allow(clippy::unwrap_used)]
    CString::new(path).unwrap()
}

pub fn create_anon_pair() -> anyhow::Result<(AgentRemoteConfigWriter<ShmHandle>, ShmHandle)> {
    let (writer, handle) = libdd_ipc::one_way_shared_memory::create_anon_pair()?;
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
    AgentRemoteConfigReader(OneWayShmReader::new_with_opener(
        try_open_shm(endpoint),
        Some(AgentRemoteConfigEndpoint(endpoint.clone())),
        |extra| {
            extra
                .as_ref()
                .and_then(|endpoint| try_open_shm(&endpoint.0))
        },
    ))
}

pub fn reader_from_shm(handle: ShmHandle) -> io::Result<AgentRemoteConfigReader<ShmHandle>> {
    Ok(AgentRemoteConfigReader(OneWayShmReader::new(
        handle.map()?,
        None,
    )))
}

pub fn new_writer(endpoint: &Endpoint) -> io::Result<AgentRemoteConfigWriter<NamedShmHandle>> {
    Ok(AgentRemoteConfigWriter(
        OneWayShmWriter::<NamedShmHandle>::new(path_for_endpoint(endpoint))?,
    ))
}

impl<T: FileBackedHandle> AgentRemoteConfigReader<T> {
    pub fn read(&mut self) -> (bool, &[u8]) {
        self.0.read()
    }
}

impl<T: FileBackedHandle> AgentRemoteConfigWriter<T> {
    /// Returns `false` if the segment could not be grown to hold `contents`, in which case
    /// nothing was published and the previous payload stays current.
    pub fn write(&self, contents: &[u8]) -> bool {
        self.0.write(contents)
    }

    pub fn size(&self) -> usize {
        self.0.size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shm_paths_distinguish_endpoints() {
        assert_ne!(
            path_for_endpoint(&Endpoint::from_slice("http://agent-a:8126")),
            path_for_endpoint(&Endpoint::from_slice("http://agent-b:8126")),
        );
    }
}
