// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::RemoteConfigHandle;
use super::publication::RemoteConfigPublisher;
use libdd_ffe::cdn::{ApplyError, ConfigurationSink};
use libdd_ffe::rules_based::Configuration;
use libdd_remote_config::RemoteConfigPath;
use std::io;

// This is a local identifier, never sent to the agent or the RC service. Each
// subscription owns its mapping, so matching paths cannot mix credentials.
const CDN_CONFIG_PATH: &str = "datadog/0/FFE_FLAGS/cdn/config";

#[derive(Clone)]
pub(crate) struct CdnPublisher(RemoteConfigPublisher);

impl CdnPublisher {
    pub(crate) fn new() -> io::Result<(Self, RemoteConfigHandle)> {
        let path = RemoteConfigPath::parse(CDN_CONFIG_PATH).map_err(io::Error::other)?;
        let (publisher, handle) = RemoteConfigPublisher::new(path)?;
        Ok((Self(publisher), handle))
    }

    pub(crate) fn stopped(&self) {
        self.0.stopped();
    }
}

impl ConfigurationSink for CdnPublisher {
    fn apply(&self, configuration: Configuration) -> Result<(), ApplyError> {
        let bytes = configuration.get_flags_configuration().ok_or(ApplyError)?;
        self.0.publish(bytes.into_owned()).map_err(|_| ApplyError)
    }

    fn failed(&self) {
        self.0.failed();
    }
    fn confirmed(&self) {
        self.0.confirmed();
    }
}

#[cfg(test)]
mod tests {
    use super::super::publication::PublicationReader;
    use super::super::{RemoteConfigDeliveryState, RemoteConfigHandle};
    use super::*;
    use libdd_ffe::rules_based::UniversalFlagConfig;
    use libdd_ipc::handles::TransferHandles;
    use std::time::Duration;

    #[test]
    #[cfg_attr(miri, ignore = "requires native shared memory")]
    fn cdn_readiness_changes_preserve_the_last_good_configuration() {
        let (publisher, handle) = CdnPublisher::new().unwrap();
        let mut reader = PublicationReader::new(handle).unwrap();
        assert!(reader.read().unwrap().is_none());
        let raw =
            br#"{"createdAt":"2026-10-14T00:00:00Z","environment":{"name":"test"},"flags":{}}"#;
        let config = Configuration::from_server_response(
            UniversalFlagConfig::from_json(raw.to_vec()).unwrap(),
        );
        publisher.apply(config).unwrap();
        let initial = reader.read().unwrap().unwrap();
        assert_eq!(initial.revision, 1);
        assert_eq!(initial.state, RemoteConfigDeliveryState::Ready);
        publisher.failed();
        let stale = reader.read().unwrap().unwrap();
        assert_eq!(stale.configuration, initial.configuration);
        assert_eq!(stale.revision, initial.revision);
        assert_eq!(stale.state, RemoteConfigDeliveryState::Stale);
        publisher.confirmed();
        let recovered = reader.read().unwrap().unwrap();
        assert_eq!(recovered.state, RemoteConfigDeliveryState::Ready);
        assert_eq!(recovered.configuration, initial.configuration);
        assert_eq!(recovered.revision, initial.revision);
        publisher.stopped();
        let stopped = reader.read().unwrap().unwrap();
        assert_eq!(stopped.state, RemoteConfigDeliveryState::Stopped);
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
        let mut reader = PublicationReader::new(handle).unwrap();
        drop(publisher);
        let terminal = reader.read().unwrap().unwrap();
        assert_eq!(terminal.state, RemoteConfigDeliveryState::Stopped);
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
        let mut live_reader = PublicationReader::new(live_handle).unwrap();
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
            let mut handle: RemoteConfigHandle = codec::decode(&bytes[..len]).unwrap();
            handle.receive_handles(&mut FdSource::new(fds)).unwrap();
            let descriptor: PlatformHandle<OwnedFileHandle> = handle.handle.clone().into();
            let stat = nix::sys::stat::fstat(descriptor.as_raw_fd()).unwrap();
            // There is no persistent directory entry even while the creator is alive.
            assert_eq!(
                stat.st_nlink, 0,
                "publisher allocated persistent named memory"
            );
            let mut reader = PublicationReader::new(handle).unwrap();
            connection.send_raw_blocking(vec![1], &[]).unwrap();
            connection.recv_raw_blocking(&mut bytes).unwrap();
            child.kill().unwrap();
            assert!(!child.wait().unwrap().success());
            let snapshot = reader.read().unwrap().unwrap();
            assert_eq!(snapshot.state, RemoteConfigDeliveryState::Ready);
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
        assert_eq!(retained.state, RemoteConfigDeliveryState::Stale);
        assert_eq!(retained.configuration, live_initial.configuration);
    }

    #[test]
    #[cfg_attr(miri, ignore = "requires native shared memory")]
    fn cdn_publisher_drop_reports_stopped() {
        let (publisher, handle) = CdnPublisher::new().unwrap();
        let mut reader = PublicationReader::new(handle).unwrap();
        publisher.apply(configuration(0)).unwrap();
        let initial = reader.read().unwrap().unwrap();
        assert_eq!(initial.state, RemoteConfigDeliveryState::Ready);
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
        assert_eq!(terminal.state, RemoteConfigDeliveryState::Stopped);
    }

    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore = "requires fork")]
    fn cdn_forked_publisher_drop_keeps_parent_ready() {
        let (publisher, handle) = CdnPublisher::new().unwrap();
        let mut reader = PublicationReader::new(handle).unwrap();
        publisher.apply(configuration(0)).unwrap();
        assert_eq!(
            reader.read().unwrap().unwrap().state,
            RemoteConfigDeliveryState::Ready
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
            RemoteConfigDeliveryState::Stale
        );
    }
}
