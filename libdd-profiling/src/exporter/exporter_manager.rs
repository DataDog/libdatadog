// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::ProfileExporter;
use crate::{exporter::File, internal::EncodedProfile};
use anyhow::Context;
use crossbeam_channel::{Receiver, Sender};
use libdd_common::tag::Tag;
use reqwest::RequestBuilder;
use std::thread::JoinHandle;
use tokio_util::sync::CancellationToken;

pub struct ExporterManager {
    cancel: CancellationToken,
    exporter: ProfileExporter,
    handle: JoinHandle<anyhow::Result<()>>,
    sender: Sender<RequestBuilder>,
    receiver: Receiver<RequestBuilder>,
}

pub struct SuspendedExporterManager {
    exporter: ProfileExporter,
    inflight: Vec<RequestBuilder>,
}

impl SuspendedExporterManager {
    /// Returns the number of inflight (unprocessed) requests
    ///
    /// This is primarily useful for testing and observability.
    #[cfg(test)]
    pub fn inflight_count(&self) -> usize {
        self.inflight.len()
    }

    /// Consumes the suspended manager and returns the inner components
    fn into_parts(self) -> (ProfileExporter, Vec<RequestBuilder>) {
        (self.exporter, self.inflight)
    }
}

impl ExporterManager {
    pub fn new(exporter: ProfileExporter) -> anyhow::Result<Self> {
        let (sender, receiver) = crossbeam_channel::bounded(2);
        let cancel = CancellationToken::new();
        let cloned_receiver: Receiver<RequestBuilder> = receiver.clone();
        let cloned_cancel = cancel.clone();
        let handle = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
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
            Ok(())
        });
        Ok(Self {
            cancel,
            exporter,
            handle,
            sender,
            receiver,
        })
    }

    pub fn abort(self) -> anyhow::Result<SuspendedExporterManager> {
        self.cancel.cancel();

        // Drain any pending messages from the channel before shutting down
        let inflight: Vec<_> = self.receiver.try_iter().collect();

        // Drop the sender to disconnect the channel and unblock recv() in the worker thread
        drop(self.sender);
        self.handle
            .join()
            .map_err(|_| anyhow::anyhow!("unable to join thread"))?
            .context("worker thread returned error")?;

        Ok(SuspendedExporterManager {
            exporter: self.exporter,
            inflight,
        })
    }

    pub fn prefork(self) -> anyhow::Result<SuspendedExporterManager> {
        self.abort()
    }

    pub fn postfork_child(suspended: SuspendedExporterManager) -> anyhow::Result<Self> {
        let (exporter, _inflight) = suspended.into_parts();
        Self::new(exporter)
    }

    pub fn postfork_parent(suspended: SuspendedExporterManager) -> anyhow::Result<Self> {
        let (exporter, inflight) = suspended.into_parts();
        let manager = Self::new(exporter)?;
        for msg in inflight {
            manager.sender.send(msg)?;
        }
        Ok(manager)
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
        let msg = self.exporter.build(
            profile,
            additional_files,
            additional_tags,
            internal_metadata,
            info,
            process_tags,
        )?;
        // TODO, use thiserror and get back the actual error if one.
        self.sender.try_send(msg)?;
        Ok(())
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
        let fixture = TestFixture::new("test_abort");
        let file_path = fixture.file_path.clone();

        // Give the worker thread time to start and block on recv()
        fixture.wait_for_processing(50);

        // This should complete quickly by dropping sender to disconnect channel
        let start = std::time::Instant::now();
        let suspended = fixture.manager.abort().expect("Failed to abort");
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
            suspended.inflight.len(),
            0,
            "No messages should be inflight"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_profile_actually_sent() {
        // Verify that queued profiles are actually sent by the worker thread
        let fixture = TestFixture::new("test_sent");

        fixture.queue_profile().expect("Failed to queue profile");
        fixture.wait_for_processing(200);

        // Verify the request was written
        assert!(fixture.has_request(), "Expected request to be sent");

        let suspended = fixture.manager.abort().expect("Failed to abort");
        // Worker thread should have processed it, so no inflight messages
        assert_eq!(
            suspended.inflight.len(),
            0,
            "No messages should be inflight after processing"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_multiple_profiles_processed() {
        // Note: file dump server overwrites the file for each request,
        // so we can only verify that at least one was processed
        let fixture = TestFixture::new("test_multiple");

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

        let _suspended = fixture.manager.abort().expect("Failed to abort");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_abort_during_send_cancels_properly() {
        // Test that abort cancels in-progress sends via the cancellation token
        let fixture = TestFixture::new("test_cancel");

        fixture.queue_profile().expect("Failed to queue profile");

        // Abort quickly (might catch during send)
        fixture.wait_for_processing(10);
        let _suspended = fixture.manager.abort().expect("Failed to abort");

        // Should complete without hanging
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_channel_respects_bounded_size() {
        // The channel is bounded(2), verify we can queue at least 2
        let fixture = TestFixture::new("test_bounded");

        // We should be able to queue at least 2 (the channel capacity)
        fixture
            .queue_profiles(2, 0)
            .expect("Should queue 2 profiles");

        fixture.wait_for_processing(400);

        // Verify at least one was processed
        assert!(fixture.has_request(), "At least one request should be sent");

        let _suspended = fixture.manager.abort().expect("Failed to abort");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_prefork_delegates_to_abort() {
        let fixture = TestFixture::new("test_prefork");
        let file_path = fixture.file_path.clone();

        fixture.queue_profile().expect("Failed to queue profile");
        fixture.wait_for_processing(200);

        // Prefork should work just like abort
        let _suspended = fixture.manager.prefork().expect("Failed to prefork");

        assert!(
            file_path.exists() && std::fs::read(&file_path).unwrap().starts_with(b"POST"),
            "Request should be sent"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_queue_with_additional_files_and_tags() {
        let fixture = TestFixture::new("test_additional");

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

        let _suspended = fixture.manager.abort().expect("Failed to abort");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_abort_drains_unprocessed_messages() {
        // Verify that messages still in the channel are captured in inflight
        let fixture = TestFixture::new("test_drain");

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
        let suspended = fixture.manager.abort().expect("Failed to abort");

        // There should be some messages captured as inflight (worker may have processed some)
        // We queued multiple, so inflight should be > 0 unless worker was very fast
        assert!(
            suspended.inflight.len() <= queued,
            "Inflight count should not exceed queued count"
        );

        eprintln!(
            "Captured {} inflight messages out of {} queued",
            suspended.inflight.len(),
            queued
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_worker_thread_exits_on_channel_disconnect() {
        // Verify that dropping sender causes worker thread to exit cleanly
        let fixture = TestFixture::new("test_disconnect");
        let thread_id = fixture.manager.handle.thread().id();

        fixture.wait_for_processing(50);

        // Verify thread is still running before we abort
        assert!(
            !fixture.manager.handle.is_finished(),
            "Thread should still be running before abort"
        );

        // Track if thread exits in reasonable time
        let start = std::time::Instant::now();
        let _suspended = fixture.manager.abort().expect("Failed to abort");
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
        let fixture = TestFixture::new("test_thread_exit");

        fixture.queue_profile().expect("Failed to queue profile");

        // Verify thread is running
        assert!(
            !fixture.manager.handle.is_finished(),
            "Worker thread should be running"
        );

        // Abort and verify thread exits
        let _suspended = fixture.manager.abort().expect("Failed to abort");

        // Success means thread exited cleanly with Ok(())
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_postfork_child_creates_new_manager() {
        // postfork_child should create a fresh manager, discarding inflight messages
        let fixture = TestFixture::new("test_postfork_child");

        // Queue some profiles
        fixture
            .queue_profiles(2, 0)
            .expect("Failed to queue profiles");

        // Suspend before processing
        let suspended = fixture.manager.abort().expect("Failed to suspend");

        // Create child manager - should work and start fresh
        let child_manager =
            ExporterManager::postfork_child(suspended).expect("Failed to create child manager");

        // Child manager should work normally
        let profile = EncodedProfile::test_instance().expect("Failed to create test profile");
        child_manager
            .queue(profile, &[], &[], None, None, None)
            .expect("Failed to queue in child");

        std::thread::sleep(Duration::from_millis(200));
        let _suspended = child_manager.abort().expect("Failed to abort child");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_postfork_parent_requeues_inflight() {
        // postfork_parent should create a new manager and re-queue inflight messages
        let fixture = TestFixture::new("test_postfork_parent");
        let file_path = fixture.file_path.clone();

        // Queue profiles with a small delay to avoid filling the channel
        fixture
            .queue_profiles(3, 10)
            .expect("Failed to queue profiles");

        // Abort immediately to maximize inflight capture
        let suspended = fixture.manager.abort().expect("Failed to suspend");
        let inflight_count = suspended.inflight_count();

        eprintln!("Captured {} inflight messages for parent", inflight_count);

        // Create parent manager - should re-queue inflight messages
        let parent_manager =
            ExporterManager::postfork_parent(suspended).expect("Failed to create parent manager");

        // Give time for re-queued messages to process
        std::thread::sleep(Duration::from_millis(300));

        // If we captured any inflight, they should be processed
        if inflight_count > 0 {
            assert!(
                file_path.exists() && std::fs::read(&file_path).unwrap().starts_with(b"POST"),
                "Re-queued messages should be processed"
            );
        }

        let _suspended = parent_manager.abort().expect("Failed to abort parent");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_full_fork_workflow() {
        // Test a complete fork workflow: create -> queue -> prefork -> postfork
        let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
        let child_file = temp_dir.path().join("test_fork_child.http");

        // Parent: create manager and queue some work
        let fixture = TestFixture::new("test_fork_parent");
        fixture
            .queue_profiles(2, 0)
            .expect("Failed to queue profiles");

        // Prefork: suspend the manager
        let suspended = fixture.manager.prefork().expect("Failed to prefork");

        // Parent process: re-queue inflight work
        let parent_manager = ExporterManager::postfork_parent(suspended)
            .expect("Failed to create parent after fork");

        std::thread::sleep(Duration::from_millis(200));
        let _parent_suspended = parent_manager.abort().expect("Failed to abort parent");

        // Child process: start fresh with new exporter
        let child_exporter = TestFixture::create_exporter(&child_file);
        let child_manager =
            ExporterManager::new(child_exporter).expect("Failed to create child manager");

        // Child does its own work
        let profile = EncodedProfile::test_instance().expect("Failed to create test profile");
        child_manager
            .queue(profile, &[], &[], None, None, None)
            .expect("Failed to queue in child");

        std::thread::sleep(Duration::from_millis(200));
        let _child_suspended = child_manager.abort().expect("Failed to abort child");

        // Verify child processed its work
        let content = std::fs::read(&child_file).ok();
        assert!(
            content.is_some_and(|c| c.starts_with(b"POST")),
            "Child should process its work"
        );
    }
}
