// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! CDN snapshots use the same shared-memory publication and notification
//! primitives as Remote Configuration, with a separate payload and namespace.

use super::{RemoteConfigReader, RemoteConfigWriter};
use base64::{Engine, prelude::BASE64_URL_SAFE_NO_PAD};
use bincode::Options;
use libdd_common::MutexExt;
use libdd_ffe::cdn::{ApplyError, ConfigurationSink};
use libdd_ffe::rules_based::Configuration;
use serde::{Deserialize, Serialize};
use std::ffi::{CStr, CString};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum CdnDeliveryState {
    #[default]
    Starting,
    Ready,
    Stale,
    Stopped,
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CdnSnapshot {
    /// Advances for accepted configuration, not for readiness-only changes.
    pub revision: u64,
    pub state: CdnDeliveryState,
    /// Validated UFC attributes; empty until the first accepted configuration.
    pub configuration: Vec<u8>,
}

pub struct CdnReader(RemoteConfigReader);

impl CdnReader {
    pub fn new(path: &CStr) -> Self {
        Self(RemoteConfigReader::from_path(path))
    }

    pub fn read(&mut self) -> io::Result<Option<CdnSnapshot>> {
        let (changed, bytes) = self.0.read();
        if !changed || bytes.is_empty() {
            return Ok(None);
        }
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit((10 << 20) + 1024)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map(Some)
            .map_err(|_| io::Error::other("invalid CDN snapshot"))
    }

    /// A bounded shared-memory wait. No HTTP or PHP signal handler is involved.
    pub fn wait_for_change(&mut self, timeout: Duration) {
        self.0.0.wait_for_change(timeout);
    }
}

struct Publication {
    writer: RemoteConfigWriter,
    snapshot: CdnSnapshot,
}

#[derive(Clone)]
pub(crate) struct CdnPublisher(Arc<Mutex<Publication>>);

impl CdnPublisher {
    pub(crate) fn new() -> io::Result<(Self, String)> {
        // Unique per live fetcher, including when two sidecars serve the same
        // UID. Names contain neither credentials nor an endpoint fingerprint.
        let path = format!(
            "/ddcdn{}",
            BASE64_URL_SAFE_NO_PAD.encode(rand::random::<[u8; 16]>())
        );
        let name = CString::new(path.clone()).map_err(io::Error::other)?;
        let publisher = Self(Arc::new(Mutex::new(Publication {
            writer: RemoteConfigWriter::from_path(name)?,
            snapshot: CdnSnapshot::default(),
        })));
        Ok((publisher, path))
    }

    fn update(&self, update: impl FnOnce(&mut CdnSnapshot)) -> Result<(), ApplyError> {
        let mut publication = self.0.lock_or_panic();
        let mut next = publication.snapshot.clone();
        update(&mut next);
        if next == publication.snapshot {
            return Ok(());
        }
        let bytes = bincode::serialize(&next).map_err(|_| ApplyError)?;
        if !publication.writer.write(&bytes) {
            return Err(ApplyError);
        }
        publication.snapshot = next;
        Ok(())
    }

    pub(crate) fn stopped(&self) {
        let _ = self.update(|snapshot| snapshot.state = CdnDeliveryState::Stopped);
    }
}

impl ConfigurationSink for CdnPublisher {
    fn apply(&self, configuration: Configuration) -> Result<(), ApplyError> {
        let bytes = configuration.get_flags_configuration().ok_or(ApplyError)?;
        self.update(|snapshot| {
            snapshot.configuration = bytes.into_owned();
            snapshot.revision = snapshot.revision.wrapping_add(1);
            snapshot.state = CdnDeliveryState::Ready;
        })
    }

    fn failed(&self) {
        let _ = self.update(|snapshot| {
            snapshot.state = if snapshot.configuration.is_empty() {
                CdnDeliveryState::Starting
            } else {
                CdnDeliveryState::Stale
            };
        });
    }

    fn confirmed(&self) {
        let _ = self.update(|snapshot| {
            if !snapshot.configuration.is_empty() {
                snapshot.state = CdnDeliveryState::Ready;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_ffe::rules_based::UniversalFlagConfig;

    #[test]
    #[cfg_attr(miri, ignore = "requires native shared memory")]
    fn cdn_readiness_changes_preserve_the_last_good_configuration() {
        let (publisher, path) = CdnPublisher::new().unwrap();
        let mut reader = CdnReader::new(&CString::new(path).unwrap());
        assert!(reader.read().unwrap().is_none());
        let raw =
            br#"{"createdAt":"2026-10-14T00:00:00Z","environment":{"name":"test"},"flags":{}}"#;
        let config = Configuration::from_server_response(
            UniversalFlagConfig::from_json(raw.to_vec()).unwrap(),
        );
        publisher.apply(config).unwrap();
        let initial = reader.read().unwrap().unwrap();
        assert_eq!(initial.revision, 1);
        assert_eq!(initial.state, CdnDeliveryState::Ready);
        publisher.failed();
        let stale = reader.read().unwrap().unwrap();
        assert_eq!(stale.configuration, initial.configuration);
        assert_eq!(stale.revision, initial.revision);
        assert_eq!(stale.state, CdnDeliveryState::Stale);
        publisher.confirmed();
        let recovered = reader.read().unwrap().unwrap();
        assert_eq!(recovered.state, CdnDeliveryState::Ready);
        assert_eq!(recovered.configuration, initial.configuration);
        assert_eq!(recovered.revision, initial.revision);
        publisher.stopped();
        let stopped = reader.read().unwrap().unwrap();
        assert_eq!(stopped.state, CdnDeliveryState::Stopped);
        assert_eq!(stopped.configuration, initial.configuration);
    }
}
