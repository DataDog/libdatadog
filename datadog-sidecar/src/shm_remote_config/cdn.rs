// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! CDN snapshots use the same shared-memory publication and notification
//! primitives as Remote Configuration, with a separate payload. Unix mappings
//! are anonymous and transferred over IPC, so published snapshots have no persistent names.

#[cfg(windows)]
use super::{RemoteConfigReader, RemoteConfigWriter};
#[cfg(windows)]
use base64::{Engine, prelude::BASE64_URL_SAFE_NO_PAD};
use bincode::Options;
use libdd_common::MutexExt;
use libdd_ffe::cdn::{ApplyError, ConfigurationSink};
use libdd_ffe::rules_based::Configuration;
use libdd_ipc::handles::{HandlesTransport, TransferHandles};
#[cfg(unix)]
use libdd_ipc::one_way_shared_memory::{OneWayShmReader, OneWayShmWriter, create_anon_pair};
#[cfg(unix)]
use libdd_ipc::platform::{FileBackedHandle, ShmHandle};
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use std::ffi::CString;
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

/// A connection-owned CDN publication. Obtain a new handle after reconnecting.
/// Unix uses anonymous memory; Windows names live only while mappings are open.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CdnHandle {
    #[cfg(unix)]
    handle: ShmHandle,
    #[cfg(windows)]
    path: String,
}

impl TransferHandles for CdnHandle {
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

pub struct CdnReader(SnapshotReader);

impl CdnReader {
    pub fn new(handle: CdnHandle) -> io::Result<Self> {
        #[cfg(unix)]
        return Ok(Self(OneWayShmReader::new(handle.handle.map()?, ())));
        #[cfg(windows)]
        Ok(Self(RemoteConfigReader::from_path(
            &CString::new(handle.path).map_err(io::Error::other)?,
        )))
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
        #[cfg(unix)]
        self.0.wait_for_change(timeout);
        #[cfg(windows)]
        self.0.0.wait_for_change(timeout);
    }
}

struct Publication {
    owner_pid: u32,
    writer: SnapshotWriter,
    snapshot: CdnSnapshot,
}

impl Drop for Publication {
    fn drop(&mut self) {
        if self.owner_pid != std::process::id() {
            return;
        }
        // Runtime teardown may abort the task before its explicit stopped() call.
        if self.snapshot.state != CdnDeliveryState::Stopped {
            self.snapshot.state = CdnDeliveryState::Stopped;
            if let Ok(bytes) = bincode::serialize(&self.snapshot) {
                let _ = self.writer.write(&bytes);
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct CdnPublisher(Arc<Mutex<Publication>>);

impl CdnPublisher {
    pub(crate) fn new() -> io::Result<(Self, CdnHandle)> {
        #[cfg(unix)]
        let (writer, handle) = {
            let (writer, handle) = create_anon_pair().map_err(io::Error::other)?;
            (writer, CdnHandle { handle })
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
            (writer, CdnHandle { path })
        };
        let publisher = Self(Arc::new(Mutex::new(Publication {
            owner_pid: std::process::id(),
            writer,
            snapshot: CdnSnapshot::default(),
        })));
        Ok((publisher, handle))
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
        let (publisher, handle) = CdnPublisher::new().unwrap();
        let mut reader = CdnReader::new(handle).unwrap();
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
    fn configuration(padding: usize) -> Configuration {
        let raw = format!(
            r#"{{"createdAt":"2026-10-14T00:00:00Z","environment":{{"name":"test"}},"flags":{{}},"padding":"{}"}}"#,
            "x".repeat(padding)
        );
        Configuration::from_server_response(
            UniversalFlagConfig::from_json(raw.into_bytes()).unwrap(),
        )
    }

    #[test]
    #[cfg_attr(miri, ignore = "requires native shared memory")]
    fn cdn_drop_before_first_poll_reports_stopped() {
        let (publisher, handle) = CdnPublisher::new().unwrap();
        let mut reader = CdnReader::new(handle).unwrap();
        drop(publisher);
        let terminal = reader.read().unwrap().unwrap();
        assert_eq!(terminal.state, CdnDeliveryState::Stopped);
        assert_eq!(terminal.revision, 0);
        assert!(terminal.configuration.is_empty());
    }

    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore = "requires native IPC")]
    fn cdn_anonymous_publisher_child() {
        use libdd_ipc::{SeqpacketConn, codec, handles::FdSink};
        let Ok(path) = std::env::var("DD_CDN_TEST_SOCKET") else {
            return;
        };
        let mut connection = SeqpacketConn::connect(path).unwrap();
        connection
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let (publisher, handle) = CdnPublisher::new().unwrap();
        let mut sink = FdSink::new();
        handle.copy_handles(&mut sink).unwrap();
        connection
            .send_raw_blocking(codec::encode(&handle), sink.fds())
            .unwrap();
        let mut ack = [0; 32];
        // The reader maps the initial 4 KiB segment before it grows.
        connection.recv_raw_blocking(&mut ack).unwrap();
        publisher.apply(configuration((10 << 20) - 1024)).unwrap();
        connection.send_raw_blocking(vec![1], &[]).unwrap();
        // The parent kills this process with the publisher still live.
        connection.recv_raw_blocking(&mut ack).unwrap();
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore = "requires child processes and native IPC")]
    fn cdn_crashed_publishers_leave_no_named_segments() {
        use libdd_ipc::platform::{OwnedFileHandle, PlatformHandle};
        use libdd_ipc::{SeqpacketListener, codec, handles::FdSource};
        use std::os::fd::AsRawFd;
        use std::process::Command;
        use std::time::Instant;

        // A live publication must remain isolated from each crashing publisher.
        let (live, live_handle) = CdnPublisher::new().unwrap();
        let mut live_reader = CdnReader::new(live_handle).unwrap();
        live.apply(configuration(0)).unwrap();
        let live_initial = live_reader.read().unwrap().unwrap();
        for _ in 0..3 {
            let directory = tempfile::tempdir().unwrap();
            let socket = directory.path().join("cdn.sock");
            let listener = SeqpacketListener::bind(&socket).unwrap();
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "shm_remote_config::cdn::tests::cdn_anonymous_publisher_child",
                    "--nocapture",
                ])
                .env("DD_CDN_TEST_SOCKET", &socket)
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut connection = loop {
                match listener.try_accept() {
                    Ok(connection) => break connection,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            let _ = child.kill();
                            let _ = child.wait();
                            panic!("publisher did not connect");
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(e) => panic!("accept failed: {e}"),
                }
            };
            connection
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut bytes = [0; 1024];
            let (len, fds) = connection.recv_raw_blocking(&mut bytes).unwrap();
            let mut handle: CdnHandle = codec::decode(&bytes[..len]).unwrap();
            handle.receive_handles(&mut FdSource::new(fds)).unwrap();
            let descriptor: PlatformHandle<OwnedFileHandle> = handle.handle.clone().into();
            let stat = nix::sys::stat::fstat(descriptor.as_raw_fd()).unwrap();
            // There is no persistent directory entry even while the creator is alive.
            assert_eq!(
                stat.st_nlink, 0,
                "publisher allocated persistent named memory"
            );
            let mut reader = CdnReader::new(handle).unwrap();
            connection.send_raw_blocking(vec![1], &[]).unwrap();
            connection.recv_raw_blocking(&mut bytes).unwrap();
            child.kill().unwrap();
            assert!(!child.wait().unwrap().success());
            let snapshot = reader.read().unwrap().unwrap();
            assert_eq!(snapshot.state, CdnDeliveryState::Ready);
            assert!(snapshot.configuration.len() > (9 << 20));
            assert_eq!(
                nix::sys::stat::fstat(descriptor.as_raw_fd())
                    .unwrap()
                    .st_nlink,
                0
            );
            // The last mappings/descriptors now release the anonymous allocation.
            drop((reader, descriptor, connection));
            assert!(live_reader.read().unwrap().is_none());
        }
        live.failed();
        let retained = live_reader.read().unwrap().unwrap();
        assert_eq!(retained.state, CdnDeliveryState::Stale);
        assert_eq!(retained.configuration, live_initial.configuration);
    }

    #[test]
    #[cfg_attr(miri, ignore = "requires native shared memory")]
    fn cdn_publisher_drop_reports_stopped() {
        let (publisher, handle) = CdnPublisher::new().unwrap();
        let mut reader = CdnReader::new(handle).unwrap();
        publisher.apply(configuration(0)).unwrap();
        let initial = reader.read().unwrap().unwrap();
        assert_eq!(initial.state, CdnDeliveryState::Ready);
        let last_owner = publisher.clone();
        drop(publisher);
        assert!(
            reader.read().unwrap().is_none(),
            "dropping one clone must not stop delivery"
        );
        let unwind = std::thread::spawn(move || {
            let _last_owner = last_owner;
            panic!("controlled owner unwind");
        })
        .join();
        assert!(unwind.is_err());
        let terminal = reader.read().unwrap().unwrap();
        assert_eq!(terminal.configuration, initial.configuration);
        assert_eq!(terminal.revision, initial.revision);
        assert_eq!(terminal.state, CdnDeliveryState::Stopped);
    }

    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore = "requires fork")]
    fn cdn_forked_publisher_drop_keeps_parent_ready() {
        let (publisher, handle) = CdnPublisher::new().unwrap();
        let mut reader = CdnReader::new(handle).unwrap();
        publisher.apply(configuration(0)).unwrap();
        assert_eq!(
            reader.read().unwrap().unwrap().state,
            CdnDeliveryState::Ready
        );
        match unsafe { libc::fork() } {
            -1 => panic!("fork failed"),
            0 => {
                drop(publisher);
                unsafe { libc::_exit(0) }
            }
            pid => {
                let mut status = 0;
                assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
                assert!(libc::WIFEXITED(status));
                assert_eq!(libc::WEXITSTATUS(status), 0);
            }
        }
        assert!(
            reader.read().unwrap().is_none(),
            "child Drop changed the parent's live snapshot"
        );
        publisher.failed();
        assert_eq!(
            reader.read().unwrap().unwrap().state,
            CdnDeliveryState::Stale
        );
    }
}
