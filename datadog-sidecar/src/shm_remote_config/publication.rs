// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Local products publish configuration under an RC path without an agent fetch.
//! Unix payloads have anonymous, kernel-managed lifetime. The manager consumes
//! these entries through the same registry and add/remove events as agent RC.

#[cfg(windows)]
use super::{RemoteConfigReader, RemoteConfigWriter};
#[cfg(windows)]
use base64::{Engine, prelude::BASE64_URL_SAFE_NO_PAD};
use bincode::Options;
use libdd_common::MutexExt;
use libdd_ipc::handles::{HandlesTransport, TransferHandles};
#[cfg(unix)]
use libdd_ipc::one_way_shared_memory::{OneWayShmReader, OneWayShmWriter, create_anon_pair};
#[cfg(unix)]
use libdd_ipc::platform::{FileBackedHandle, ShmHandle};
use libdd_remote_config::RemoteConfigPath;
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use std::ffi::CString;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum RemoteConfigDeliveryState {
    #[default]
    Starting,
    Ready,
    Stale,
    Stopped,
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct PublicationSnapshot {
    /// Advances for accepted configuration, not for readiness-only changes.
    pub revision: u64,
    pub state: RemoteConfigDeliveryState,
    /// Product payload, without its transport envelope; empty before first publication.
    pub configuration: Vec<u8>,
}

/// A connection-owned local RC publication. Obtain a new handle after reconnecting.
/// Unix uses anonymous memory; Windows names live only while mappings are open.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteConfigHandle {
    config_path: String,
    #[cfg(unix)]
    pub(super) handle: ShmHandle,
    #[cfg(windows)]
    path: String,
}

impl TransferHandles for RemoteConfigHandle {
    fn copy_handles<T: HandlesTransport>(&self, transport: T) -> Result<(), T::Error> {
        #[cfg(unix)]
        return self.handle.copy_handles(transport);
        #[cfg(windows)]
        {
            let _ = transport;
            Ok(())
        }
    }

    fn receive_handles<T: HandlesTransport>(&mut self, transport: T) -> Result<(), T::Error> {
        #[cfg(unix)]
        return self.handle.receive_handles(transport);
        #[cfg(windows)]
        {
            let _ = transport;
            Ok(())
        }
    }
}

#[cfg(unix)]
type SnapshotReader = OneWayShmReader<ShmHandle, ()>;
#[cfg(windows)]
type SnapshotReader = RemoteConfigReader;
#[cfg(unix)]
type SnapshotWriter = OneWayShmWriter<ShmHandle>;
#[cfg(windows)]
type SnapshotWriter = RemoteConfigWriter;

pub(super) struct PublicationReader {
    reader: SnapshotReader,
    pub path: RemoteConfigPath,
}

impl PublicationReader {
    pub fn new(handle: RemoteConfigHandle) -> io::Result<Self> {
        let path = RemoteConfigPath::parse(&handle.config_path).map_err(io::Error::other)?;
        #[cfg(unix)]
        let reader = OneWayShmReader::new(handle.handle.map()?, ());
        #[cfg(windows)]
        let reader =
            RemoteConfigReader::from_path(&CString::new(handle.path).map_err(io::Error::other)?);
        Ok(Self { reader, path })
    }

    pub fn read(&mut self) -> io::Result<Option<PublicationSnapshot>> {
        let (changed, bytes) = self.reader.read();
        if !changed || bytes.is_empty() {
            return Ok(None);
        }
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit((10 << 20) + 1024)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map(Some)
            .map_err(|_| io::Error::other("invalid local RC snapshot"))
    }

    /// A bounded shared-memory wait. No HTTP or PHP signal handler is involved.
    pub fn wait_for_change(&mut self, timeout: Duration) {
        #[cfg(unix)]
        self.reader.wait_for_change(timeout);
        #[cfg(windows)]
        self.reader.0.wait_for_change(timeout);
    }
}

struct Publication {
    owner_pid: u32,
    writer: SnapshotWriter,
    snapshot: PublicationSnapshot,
}

impl Drop for Publication {
    fn drop(&mut self) {
        if self.owner_pid != std::process::id() {
            return;
        }
        // Runtime teardown may abort the task before its explicit stopped() call.
        if self.snapshot.state != RemoteConfigDeliveryState::Stopped {
            self.snapshot.state = RemoteConfigDeliveryState::Stopped;
            if let Ok(bytes) = bincode::serialize(&self.snapshot) {
                let _ = self.writer.write(&bytes);
            }
        }
    }
}

#[derive(Clone)]
pub(super) struct RemoteConfigPublisher(Arc<Mutex<Publication>>);

impl RemoteConfigPublisher {
    pub(super) fn new(path: RemoteConfigPath) -> io::Result<(Self, RemoteConfigHandle)> {
        let config_path = path.to_string();
        #[cfg(unix)]
        let (writer, handle) = {
            let (writer, handle) = create_anon_pair().map_err(io::Error::other)?;
            (
                writer,
                RemoteConfigHandle {
                    handle,
                    config_path,
                },
            )
        };
        #[cfg(windows)]
        let (writer, handle) = {
            // Windows mappings have kernel-managed lifetime; do not duplicate handles
            // into a client that might disconnect before reading its reply.
            let path = format!(
                "/ddcdn{}",
                BASE64_URL_SAFE_NO_PAD.encode(rand::random::<[u8; 16]>())
            );
            let writer = RemoteConfigWriter::from_path(
                CString::new(path.clone()).map_err(io::Error::other)?,
            )?;
            (writer, RemoteConfigHandle { path, config_path })
        };
        let publisher = Self(Arc::new(Mutex::new(Publication {
            owner_pid: std::process::id(),
            writer,
            snapshot: PublicationSnapshot::default(),
        })));
        Ok((publisher, handle))
    }

    fn update(&self, update: impl FnOnce(&mut PublicationSnapshot)) -> io::Result<()> {
        let mut publication = self.0.lock_or_panic();
        let mut next = publication.snapshot.clone();
        update(&mut next);
        if next == publication.snapshot {
            return Ok(());
        }
        let bytes = bincode::serialize(&next).map_err(io::Error::other)?;
        if !publication.writer.write(&bytes) {
            return Err(io::Error::other("failed to publish local RC configuration"));
        }
        publication.snapshot = next;
        Ok(())
    }

    pub(crate) fn stopped(&self) {
        let _ = self.update(|snapshot| snapshot.state = RemoteConfigDeliveryState::Stopped);
    }
}

impl RemoteConfigPublisher {
    pub(super) fn publish(&self, bytes: Vec<u8>) -> io::Result<()> {
        self.update(|snapshot| {
            snapshot.configuration = bytes;
            snapshot.revision = snapshot.revision.wrapping_add(1);
            snapshot.state = RemoteConfigDeliveryState::Ready;
        })
    }

    pub(super) fn failed(&self) {
        let _ = self.update(|snapshot| {
            snapshot.state = if snapshot.configuration.is_empty() {
                RemoteConfigDeliveryState::Starting
            } else {
                RemoteConfigDeliveryState::Stale
            };
        });
    }

    pub(super) fn confirmed(&self) {
        let _ = self.update(|snapshot| {
            if !snapshot.configuration.is_empty() {
                snapshot.state = RemoteConfigDeliveryState::Ready;
            }
        });
    }
}
