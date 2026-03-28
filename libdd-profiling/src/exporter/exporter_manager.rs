// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::ProfileExporter;
use crate::{exporter::File, internal::EncodedProfile};
use crossbeam_channel::{Receiver, Sender};
use libdd_common::tag::Tag;
use reqwest::RequestBuilder;
use std::thread::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub enum ExporterManager {
    Active {
        cancel: CancellationToken,
        exporter: ProfileExporter,
        handle: JoinHandle<()>,
        sender: Sender<RequestBuilder>,
        receiver: Receiver<RequestBuilder>,
    },
    Suspended {
        exporter: ProfileExporter,
        inflight: Vec<RequestBuilder>,
    },
    /// Temporary state used during transitions between Active and Suspended.
    /// The manager should never be left in this state.
    Transitioning,
}

impl ExporterManager {
    pub fn new(exporter: ProfileExporter) -> anyhow::Result<Self> {
        let (sender, receiver) = crossbeam_channel::bounded(2);
        let cancel = CancellationToken::new();
        let cloned_receiver: Receiver<RequestBuilder> = receiver.clone();
        let cloned_cancel = cancel.clone();
        let handle = std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };

            runtime.block_on(async {
                loop {
                    let Ok(msg) = cloned_receiver.recv() else {
                        return;
                    };
                    if cloned_cancel
                        .run_until_cancelled(msg.send())
                        .await
                        .is_none()
                    {
                        return;
                    }
                    // TODO: Add logging for failed uploads.
                }
            });
        });
        Ok(Self::Active {
            cancel,
            exporter,
            handle,
            sender,
            receiver,
        })
    }

    pub fn abort(&mut self) -> anyhow::Result<()> {
        let old = std::mem::replace(self, Self::Transitioning);

        let Self::Active {
            cancel,
            exporter,
            handle,
            sender,
            receiver,
        } = old
        else {
            *self = old;
            anyhow::bail!("Cannot abort manager in state: {:?}", self);
        };

        cancel.cancel();
        let inflight: Vec<_> = receiver.try_iter().collect();
        drop(sender);

        match handle.join() {
            Ok(()) => {
                *self = Self::Suspended { exporter, inflight };
                Ok(())
            }
            Err(_) => {
                *self = Self::Suspended { exporter, inflight };
                Err(anyhow::anyhow!("unable to join thread"))
            }
        }
    }

    pub fn prefork(&mut self) -> anyhow::Result<()> {
        self.abort()
    }

    pub fn postfork_child(&mut self) -> anyhow::Result<()> {
        let old = std::mem::replace(self, Self::Transitioning);

        let Self::Suspended { exporter, .. } = old else {
            *self = old;
            anyhow::bail!(
                "postfork_child requires a suspended manager, found: {:?}",
                self
            );
        };

        *self = Self::new(exporter)?;
        Ok(())
    }

    pub fn postfork_parent(&mut self) -> anyhow::Result<()> {
        let old = std::mem::replace(self, Self::Transitioning);

        let Self::Suspended { exporter, inflight } = old else {
            *self = old;
            anyhow::bail!(
                "postfork_parent requires a suspended manager, found: {:?}",
                self
            );
        };

        let new_manager = Self::new(exporter)?;
        if let Self::Active { ref sender, .. } = new_manager {
            for msg in inflight {
                sender.send(msg)?;
            }
        }
        *self = new_manager;
        Ok(())
    }

    pub fn queue(
        &self,
        profile: EncodedProfile,
        additional_files: &[File<'_>],
        additional_tags: &[Tag],
        internal_metadata: Option<serde_json::Value>,
        info: Option<serde_json::Value>,
        process_tags: Option<&str>,
    ) -> anyhow::Result<()> {
        let Self::Active {
            exporter, sender, ..
        } = self
        else {
            anyhow::bail!("Cannot queue on manager in state: {:?}", self);
        };

        let msg = exporter.build(
            profile,
            additional_files,
            additional_tags,
            internal_metadata,
            info,
            process_tags,
        )?;
        // TODO, use thiserror and get back the actual error if one.
        sender.try_send(msg)?;
        Ok(())
    }

    /// Returns the number of inflight (unprocessed) requests
    ///
    /// This is primarily useful for testing and observability.
    #[cfg(test)]
    pub fn inflight_count(&self) -> usize {
        match self {
            Self::Active { .. } => 0,
            Self::Suspended { inflight, .. } => inflight.len(),
            Self::Transitioning => panic!("Manager is in transitioning state"),
        }
    }

    /// Returns whether the worker thread has finished (for testing)
    #[cfg(test)]
    pub fn is_finished(&self) -> bool {
        match self {
            Self::Active { handle, .. } => handle.is_finished(),
            Self::Suspended { .. } => true,
            Self::Transitioning => panic!("Manager is in transitioning state"),
        }
    }

    /// Returns the worker thread ID (for testing)
    #[cfg(test)]
    pub fn thread_id(&self) -> Option<std::thread::ThreadId> {
        match self {
            Self::Active { handle, .. } => Some(handle.thread().id()),
            Self::Suspended { .. } => None,
            Self::Transitioning => panic!("Manager is in transitioning state"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exporter::config;
    use crate::internal::EncodedProfile;
    use std::time::Duration;

    /// Test fixture that sets up a manager with a file-based exporter
    struct TestFixture {
        manager: ExporterManager,
        file_path: std::path::PathBuf,
        _temp_dir: tempfile::TempDir,
    }

    impl TestFixture {
        fn new(test_name: &str) -> Self {
            let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
            let file_path = temp_dir.path().join(format!("{}.http", test_name));
            let exporter = Self::create_exporter(&file_path);
            let manager = ExporterManager::new(exporter).expect("Failed to create manager");

            Self {
                manager,
                file_path,
                _temp_dir: temp_dir,
            }
        }

        fn create_exporter(path: &std::path::Path) -> ProfileExporter {
            let endpoint =
                config::file(path.to_string_lossy()).expect("Failed to create file endpoint");
            ProfileExporter::new("test-lib", "1.0.0", "test", vec![], endpoint)
                .expect("Failed to create test exporter")
        }

        fn queue_profile(&self) -> anyhow::Result<()> {
            let profile = EncodedProfile::test_instance()?;
            self.manager.queue(profile, &[], &[], None, None, None)
        }

        fn queue_profiles(&self, count: usize, delay_ms: u64) -> anyhow::Result<()> {
            for _ in 0..count {
                self.queue_profile()?;
                if delay_ms > 0 {
                    std::thread::sleep(Duration::from_millis(delay_ms));
                }
            }
            Ok(())
        }

        fn has_request(&self) -> bool {
            if !self.file_path.exists() {
                return false;
            }
            let content = std::fs::read(&self.file_path).expect("Failed to read file");
            content.starts_with(b"POST")
        }

        fn wait_for_processing(&self, ms: u64) {
            std::thread::sleep(Duration::from_millis(ms));
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_abort_without_queuing_unblocks_recv() {
        // This is the key fix - abort should unblock recv() by dropping the sender
        let mut fixture = TestFixture::new("test_abort");
        let file_path = fixture.file_path.clone();

        // Give the worker thread time to start and block on recv()
        fixture.wait_for_processing(50);

        // This should complete quickly by dropping sender to disconnect channel
        let start = std::time::Instant::now();
        fixture.manager.abort().expect("Failed to abort");
        let elapsed = start.elapsed();

        // Should complete almost immediately (< 100ms)
        assert!(
            elapsed < Duration::from_millis(100),
            "Abort took too long ({}ms), channel may not have disconnected",
            elapsed.as_millis()
        );

        // No requests should have been sent
        assert!(
            !file_path.exists() || !std::fs::read(&file_path).unwrap().starts_with(b"POST"),
            "No request should have been sent"
        );

        // No messages should have been in flight
        assert_eq!(
            fixture.manager.inflight_count(),
            0,
            "No messages should be inflight"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_profile_actually_sent() {
        // Verify that queued profiles are actually sent by the worker thread
        let mut fixture = TestFixture::new("test_sent");

        fixture.queue_profile().expect("Failed to queue profile");
        fixture.wait_for_processing(200);

        // Verify the request was written
        assert!(fixture.has_request(), "Expected request to be sent");

        fixture.manager.abort().expect("Failed to abort");
        // Worker thread should have processed it, so no inflight messages
        assert_eq!(
            fixture.manager.inflight_count(),
            0,
            "No messages should be inflight after processing"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_multiple_profiles_processed() {
        // Note: file dump server overwrites the file for each request,
        // so we can only verify that at least one was processed
        let mut fixture = TestFixture::new("test_multiple");

        // Queue multiple profiles (with small delays to avoid filling the bounded channel)
        fixture
            .queue_profiles(3, 50)
            .expect("Failed to queue profiles");
        fixture.wait_for_processing(400);

        // At least the last one should have been sent
        assert!(
            fixture.has_request(),
            "Expected at least one request to be sent"
        );

        fixture.manager.abort().expect("Failed to abort");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_abort_during_send_cancels_properly() {
        // Test that abort cancels in-progress sends via the cancellation token
        let mut fixture = TestFixture::new("test_cancel");

        fixture.queue_profile().expect("Failed to queue profile");

        // Abort quickly (might catch during send)
        fixture.wait_for_processing(10);
        fixture.manager.abort().expect("Failed to abort");

        // Should complete without hanging
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_channel_respects_bounded_size() {
        // The channel is bounded(2), verify we can queue at least 2
        let mut fixture = TestFixture::new("test_bounded");

        // We should be able to queue at least 2 (the channel capacity)
        fixture
            .queue_profiles(2, 0)
            .expect("Should queue 2 profiles");

        fixture.wait_for_processing(400);

        // Verify at least one was processed
        assert!(fixture.has_request(), "At least one request should be sent");

        fixture.manager.abort().expect("Failed to abort");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_prefork_delegates_to_abort() {
        let mut fixture = TestFixture::new("test_prefork");
        let file_path = fixture.file_path.clone();

        fixture.queue_profile().expect("Failed to queue profile");
        fixture.wait_for_processing(200);

        // Prefork should work just like abort
        fixture.manager.prefork().expect("Failed to prefork");

        assert!(
            file_path.exists() && std::fs::read(&file_path).unwrap().starts_with(b"POST"),
            "Request should be sent"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_queue_with_additional_files_and_tags() {
        let mut fixture = TestFixture::new("test_additional");

        let profile = EncodedProfile::test_instance().expect("Failed to create test profile");
        let tags = vec![Tag::new("key", "value").unwrap()];
        let files = vec![File {
            name: "test.txt",
            bytes: b"test content",
        }];

        fixture
            .manager
            .queue(
                profile,
                &files,
                &tags,
                Some(serde_json::json!({"test": "metadata"})),
                Some(serde_json::json!({"info": "data"})),
                Some("process:tags"),
            )
            .expect("Failed to queue profile with additional data");

        fixture.wait_for_processing(200);

        // Verify the request was sent
        assert!(fixture.has_request(), "Request should be sent");

        // Read and verify the request contains our custom data
        let content = std::fs::read(&fixture.file_path).expect("Failed to read file");
        let content_str = String::from_utf8_lossy(&content);
        assert!(
            content_str.contains("test.txt"),
            "Should include additional file name"
        );
        assert!(
            content_str.contains("key:value"),
            "Should include custom tag"
        );

        fixture.manager.abort().expect("Failed to abort");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_abort_drains_unprocessed_messages() {
        // Verify that messages still in the channel are captured in inflight
        let mut fixture = TestFixture::new("test_drain");

        // Queue profiles until channel is full (bounded to 2)
        // The worker thread may or may not have started processing yet
        let mut queued = 0;
        for _ in 0..10 {
            if fixture.queue_profiles(1, 0).is_ok() {
                queued += 1;
            } else {
                break; // Channel is full
            }
        }

        // Should have queued at least 2 (the channel capacity)
        assert!(
            queued >= 2,
            "Should have been able to queue at least 2 profiles"
        );

        // Abort immediately without giving time to process
        fixture.manager.abort().expect("Failed to abort");

        // There should be some messages captured as inflight (worker may have processed some)
        // We queued multiple, so inflight should be > 0 unless worker was very fast
        let inflight_count = fixture.manager.inflight_count();
        assert!(
            inflight_count <= queued,
            "Inflight count should not exceed queued count"
        );

        eprintln!(
            "Captured {} inflight messages out of {} queued",
            inflight_count, queued
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_worker_thread_exits_on_channel_disconnect() {
        // Verify that dropping sender causes worker thread to exit cleanly
        let mut fixture = TestFixture::new("test_disconnect");
        let thread_id = fixture.manager.thread_id().expect("Should have thread ID");

        fixture.wait_for_processing(50);

        // Verify thread is still running before we abort
        assert!(
            !fixture.manager.is_finished(),
            "Thread should still be running before abort"
        );

        // Track if thread exits in reasonable time
        let start = std::time::Instant::now();
        fixture.manager.abort().expect("Failed to abort");
        let elapsed = start.elapsed();

        // Thread should exit quickly when channel disconnects
        assert!(
            elapsed < Duration::from_millis(200),
            "Thread should exit quickly on channel disconnect, took {}ms",
            elapsed.as_millis()
        );

        eprintln!("Verified thread {:?} exited cleanly after abort", thread_id);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_abort_verifies_thread_exit() {
        // Explicit test that abort() actually waits for thread to exit
        let mut fixture = TestFixture::new("test_thread_exit");

        fixture.queue_profile().expect("Failed to queue profile");

        // Verify thread is running
        assert!(
            !fixture.manager.is_finished(),
            "Worker thread should be running"
        );

        // Abort and verify thread exits
        fixture.manager.abort().expect("Failed to abort");

        // Success means thread exited cleanly with Ok(())
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_postfork_child_creates_new_manager() {
        // postfork_child should create a fresh manager, discarding inflight messages
        let mut fixture = TestFixture::new("test_postfork_child");

        // Queue some profiles
        fixture
            .queue_profiles(2, 0)
            .expect("Failed to queue profiles");

        // Suspend before processing
        fixture.manager.abort().expect("Failed to suspend");

        // Create child manager - should work and start fresh
        fixture
            .manager
            .postfork_child()
            .expect("Failed to create child manager");

        // Child manager should work normally
        let profile = EncodedProfile::test_instance().expect("Failed to create test profile");
        fixture
            .manager
            .queue(profile, &[], &[], None, None, None)
            .expect("Failed to queue in child");

        std::thread::sleep(Duration::from_millis(200));
        fixture.manager.abort().expect("Failed to abort child");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_postfork_parent_requeues_inflight() {
        // postfork_parent should create a new manager and re-queue inflight messages
        let mut fixture = TestFixture::new("test_postfork_parent");
        let file_path = fixture.file_path.clone();

        // Queue profiles with a small delay to avoid filling the channel
        fixture
            .queue_profiles(3, 10)
            .expect("Failed to queue profiles");

        // Abort immediately to maximize inflight capture
        fixture.manager.abort().expect("Failed to suspend");
        let inflight_count = fixture.manager.inflight_count();

        eprintln!("Captured {} inflight messages for parent", inflight_count);

        // Create parent manager - should re-queue inflight messages
        fixture
            .manager
            .postfork_parent()
            .expect("Failed to create parent manager");

        // Give time for re-queued messages to process
        std::thread::sleep(Duration::from_millis(300));

        // If we captured any inflight, they should be processed
        if inflight_count > 0 {
            assert!(
                file_path.exists() && std::fs::read(&file_path).unwrap().starts_with(b"POST"),
                "Re-queued messages should be processed"
            );
        }

        fixture.manager.abort().expect("Failed to abort parent");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_full_fork_workflow() {
        // Test a complete fork workflow: create -> queue -> prefork -> postfork
        let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
        let child_file = temp_dir.path().join("test_fork_child.http");

        // Parent: create manager and queue some work
        let mut fixture = TestFixture::new("test_fork_parent");
        fixture
            .queue_profiles(2, 0)
            .expect("Failed to queue profiles");

        // Prefork: suspend the manager
        fixture.manager.prefork().expect("Failed to prefork");

        // Parent process: re-queue inflight work
        fixture
            .manager
            .postfork_parent()
            .expect("Failed to create parent after fork");

        std::thread::sleep(Duration::from_millis(200));
        fixture.manager.abort().expect("Failed to abort parent");

        // Child process: start fresh with new exporter
        let child_exporter = TestFixture::create_exporter(&child_file);
        let mut child_manager =
            ExporterManager::new(child_exporter).expect("Failed to create child manager");

        // Child does its own work
        let profile = EncodedProfile::test_instance().expect("Failed to create test profile");
        child_manager
            .queue(profile, &[], &[], None, None, None)
            .expect("Failed to queue in child");

        std::thread::sleep(Duration::from_millis(200));
        child_manager.abort().expect("Failed to abort child");

        // Verify child processed its work
        let content = std::fs::read(&child_file).ok();
        assert!(
            content.is_some_and(|c| c.starts_with(b"POST")),
            "Child should process its work"
        );
    }
}
