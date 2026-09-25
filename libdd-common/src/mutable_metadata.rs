// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Runtime metadata that the host tracer may update after startup.
//!
//! `runtime_id` and `process_tags` are the two tracer metadata values that may change
//! while exporters and workers are already running. They live in a single
//! [`MutableMetadata`] struct shared through an [`ArcSwap`]; every component holds a
//! [`MutableMetadataHandle`] and reads the values through it at use time, so an update
//! by the host SDK propagates to all components.
//!
//! # Protocol
//!
//! - **Writers** (the host SDK via FFI, or the library itself) call
//!   [`MutableMetadataHandle::set_runtime_id`] / [`MutableMetadataHandle::set_process_tags`] /
//!   [`MutableMetadataHandle::update`] to modify the current snapshot.
//! - **Readers** call [`MutableMetadataHandle::load`] at the point of use, which returns a
//!   lock-free snapshot. The snapshot is consistent for a single read only; values are **not**
//!   consistent across multiple `load()` calls.

use alloc::sync::Arc;

use arc_swap::ArcSwap;

/// The tracer metadata values that may be updated after startup.
///
/// Both fields use the same formats as the rest of the tracing stack:
/// - `runtime_id`: a runtime UUID,
/// - `process_tags`: comma-separated `key:value` pairs, e.g. `"k1:v1,k2:v2"`.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct MutableMetadata {
    /// Runtime UUID. Uniquely identifies the runtime within the host process.
    pub runtime_id: String,
    /// Comma-separated `key:value` process tags of the instrumented application.
    pub process_tags: String,
}

/// A shared handle to a [`MutableMetadata`].
///
/// This is a cheap-to-clone `Arc<ArcSwap<MutableMetadata>>`: all clones observe the same
/// underlying value. See the
/// [module documentation](self) for the write/read protocol.
#[derive(Clone, Default, Debug)]
pub struct MutableMetadataHandle(Arc<ArcSwap<MutableMetadata>>);

impl MutableMetadataHandle {
    /// Load the value of the handle
    pub fn load(&self) -> arc_swap::Guard<Arc<MutableMetadata>> {
        self.0.load()
    }

    /// Load the value of the handle, cloned into an owning [`Arc`].
    pub fn load_full(&self) -> Arc<MutableMetadata> {
        self.0.load_full()
    }

    /// Set the runtime_id value
    pub fn set_runtime_id(&self, runtime_id: String) {
        self.update(|mut metadata| {
            metadata.runtime_id = runtime_id.clone();
            metadata
        });
    }

    /// Set the process tags value
    pub fn set_process_tags(&self, process_tags: String) {
        self.update(|mut metadata| {
            metadata.process_tags = process_tags.clone();
            metadata
        });
    }

    /// Update the handle value with the result of `f`.
    ///
    /// Use this method to update multiple fields at once. Modify the supplied snapshot
    /// rather than replacing it with a previously loaded value. The closure may run
    /// more than once when another writer updates the handle concurrently.
    pub fn update<F>(&self, mut f: F)
    where
        F: FnMut(MutableMetadata) -> MutableMetadata,
    {
        self.0.rcu(|current| {
            let next = (**current).clone();
            f(next)
        });
    }
}

impl From<MutableMetadata> for MutableMetadataHandle {
    fn from(value: MutableMetadata) -> Self {
        MutableMetadataHandle(Arc::new(ArcSwap::from_pointee(value)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn initial_handle() -> MutableMetadataHandle {
        let handle = MutableMetadataHandle::default();
        handle.set_runtime_id("rt-1".into());
        handle.set_process_tags("k1:v1".into());
        handle
    }

    #[test]
    fn handle_propagates_updates_to_all_clones() {
        let handle = initial_handle();
        let clone = handle.clone();

        // Readers see the initial values through both clones.
        let snapshot = handle.load();
        assert_eq!(snapshot.runtime_id, "rt-1");
        assert_eq!(snapshot.process_tags, "k1:v1");
        drop(snapshot);

        // Updating through one clone propagates to the other.
        clone.update(|mut metadata| {
            metadata.runtime_id = "rt-2".into();
            metadata.process_tags = "k2:v2".into();
            metadata
        });
        let snapshot = handle.load();
        assert_eq!(snapshot.runtime_id, "rt-2");
        assert_eq!(snapshot.process_tags, "k2:v2");
    }

    #[test]
    fn field_updates_keep_the_other_field() {
        let handle = initial_handle();

        handle.set_runtime_id("rt-2".into());
        let snapshot = handle.load();
        assert_eq!(snapshot.runtime_id, "rt-2");
        assert_eq!(snapshot.process_tags, "k1:v1");
        drop(snapshot);

        handle.set_process_tags("k2:v2".into());
        let snapshot = handle.load();
        assert_eq!(snapshot.runtime_id, "rt-2");
        assert_eq!(snapshot.process_tags, "k2:v2");
    }

    #[test]
    fn default_handle_yields_empty_values() {
        let handle = MutableMetadataHandle::default();
        let snapshot = handle.load();
        assert!(snapshot.runtime_id.is_empty());
        assert!(snapshot.process_tags.is_empty());
    }

    #[test]
    fn update_modifies_the_current_snapshot() {
        let handle = initial_handle();

        handle.update(|mut m| {
            m.runtime_id = "rt-2".into();
            m
        });
        let snapshot = handle.load();
        assert_eq!(snapshot.runtime_id, "rt-2");
        assert_eq!(snapshot.process_tags, "k1:v1");
    }

    #[test]
    fn update_retries_without_losing_concurrent_changes() {
        use core::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Barrier;

        let handle = initial_handle();
        let snapshot_loaded = Barrier::new(2);
        let concurrent_update_done = Barrier::new(2);
        let first_attempt = AtomicBool::new(true);

        std::thread::scope(|scope| {
            let writer = scope.spawn(|| {
                handle.update(|mut metadata| {
                    if first_attempt.swap(false, Ordering::SeqCst) {
                        snapshot_loaded.wait();
                        concurrent_update_done.wait();
                    }
                    metadata.runtime_id = "rt-2".into();
                    metadata
                });
            });
            snapshot_loaded.wait();
            handle.set_process_tags("k2:v2".into());
            concurrent_update_done.wait();
            writer.join().unwrap();
        });

        let snapshot = handle.load();
        assert_eq!(snapshot.runtime_id, "rt-2");
        assert_eq!(snapshot.process_tags, "k2:v2");
    }

    #[test]
    fn load_full_returns_an_owning_arc() {
        let handle = initial_handle();
        let snapshot = handle.load_full();
        assert_eq!(snapshot.runtime_id, "rt-1");
        assert_eq!(snapshot.process_tags, "k1:v1");
    }
}
