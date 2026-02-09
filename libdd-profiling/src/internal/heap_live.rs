// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::api;
use crate::api::ManagedStringId;
use crate::collections::string_storage::ManagedStringStorage;
use crate::internal::owned_types::{OwnedFrame, OwnedLabel};
use std::collections::HashMap;
use std::hash::BuildHasherDefault;

/// Tracks live heap allocations inside a Profile.
/// Stores owned copies of sample data (frames, labels, values) so tracked
/// allocations survive across profile resets. Injected into the Profile's
/// observations automatically before serialization via
/// `reset_and_return_previous()`.
type FxBuildHasher = BuildHasherDefault<rustc_hash::FxHasher>;

/// Indices into the sample values array for reading/writing heap-live fields.
struct ValueIndices {
    /// Index of alloc-size in the sample values array (to read allocation size).
    alloc_size: usize,
    /// Index of heap-live-samples in the sample values array (to write 1).
    heap_live_samples: usize,
    /// Index of heap-live-size in the sample values array (to write the size).
    heap_live_size: usize,
    /// Total number of values per sample.
    num_values: usize,
}

impl ValueIndices {
    /// Build a heap-live values vector: all zeros except heap-live-samples=1
    /// and heap-live-size=alloc_size (read from the original sample).
    fn build_values(&self, sample_values: &[i64]) -> Vec<i64> {
        let alloc_size = sample_values.get(self.alloc_size).copied().unwrap_or(0);
        let mut values = vec![0i64; self.num_values];
        values[self.heap_live_samples] = 1;
        values[self.heap_live_size] = alloc_size;
        values
    }
}

pub(crate) struct HeapLiveState {
    pub tracked: HashMap<u64, TrackedAlloc, FxBuildHasher>,
    pub max_tracked: usize,
    pub excluded_labels: Vec<Box<str>>,
    indices: ValueIndices,
}

/// A single tracked live allocation with owned frame/label/values data.
pub(crate) struct TrackedAlloc {
    pub frames: Vec<OwnedFrame>,
    pub labels: Vec<OwnedLabel>,
    pub values: Vec<i64>,
}

impl HeapLiveState {
    /// Create a new heap-live tracker.
    ///
    /// # Panics
    /// Panics if any of the value indices (`alloc_size_idx`,
    /// `heap_live_samples_idx`, `heap_live_size_idx`) are >= `num_values`.
    pub fn new(
        max_tracked: usize,
        excluded_labels: &[&str],
        alloc_size_idx: usize,
        heap_live_samples_idx: usize,
        heap_live_size_idx: usize,
        num_values: usize,
    ) -> Self {
        assert!(
            alloc_size_idx < num_values,
            "alloc_size_idx ({alloc_size_idx}) must be < num_values ({num_values})"
        );
        assert!(
            heap_live_samples_idx < num_values,
            "heap_live_samples_idx ({heap_live_samples_idx}) must be < num_values ({num_values})"
        );
        assert!(
            heap_live_size_idx < num_values,
            "heap_live_size_idx ({heap_live_size_idx}) must be < num_values ({num_values})"
        );
        Self {
            tracked: HashMap::with_capacity_and_hasher(max_tracked, FxBuildHasher::default()),
            max_tracked,
            excluded_labels: excluded_labels.iter().map(|s| Box::from(*s)).collect(),
            indices: ValueIndices {
                alloc_size: alloc_size_idx,
                heap_live_samples: heap_live_samples_idx,
                heap_live_size: heap_live_size_idx,
                num_values,
            },
        }
    }

    /// Track a new allocation. Copies borrowed strings from the sample into
    /// owned storage. Constructs heap-live-only values: all zeros except
    /// heap-live-samples=1 and heap-live-size=alloc_size.
    ///
    /// Returns false if the tracker is at capacity **and** `ptr` is not
    /// already tracked. When `ptr` is already present the entry is replaced
    /// unconditionally.
    pub fn track(&mut self, ptr: u64, sample: &api::Sample) -> bool {
        if self.tracked.len() >= self.max_tracked && !self.tracked.contains_key(&ptr) {
            return false;
        }
        let alloc = TrackedAlloc::from_api_sample(sample, &self.excluded_labels, &self.indices);
        self.tracked.insert(ptr, alloc);
        true
    }

    /// Track a new allocation from a StringIdSample. Resolves ManagedStringIds
    /// via the provided string storage into owned strings.
    ///
    /// Returns false if the tracker is at capacity **and** `ptr` is not
    /// already tracked. When `ptr` is already present the entry is replaced
    /// unconditionally.
    pub fn track_string_id(
        &mut self,
        ptr: u64,
        sample: &api::StringIdSample,
        storage: &ManagedStringStorage,
    ) -> anyhow::Result<bool> {
        if self.tracked.len() >= self.max_tracked && !self.tracked.contains_key(&ptr) {
            return Ok(false);
        }
        let alloc = TrackedAlloc::from_string_id_sample(
            sample,
            storage,
            &self.excluded_labels,
            &self.indices,
        )?;
        self.tracked.insert(ptr, alloc);
        Ok(true)
    }

    /// Remove a tracked allocation. No-op if ptr is not tracked.
    pub fn untrack(&mut self, ptr: u64) {
        self.tracked.remove(&ptr);
    }
}

fn is_excluded(excluded_labels: &[Box<str>], key: &str) -> bool {
    excluded_labels.iter().any(|ex| ex.as_ref() == key)
}

impl TrackedAlloc {
    fn from_api_sample(
        sample: &api::Sample,
        excluded_labels: &[Box<str>],
        indices: &ValueIndices,
    ) -> Self {
        let frames = sample
            .locations
            .iter()
            .map(|loc| OwnedFrame {
                function_name: loc.function.name.into(),
                filename: loc.function.filename.into(),
                line: loc.line,
            })
            .collect();

        let labels = sample
            .labels
            .iter()
            .filter(|l| !is_excluded(excluded_labels, l.key))
            .map(|l| OwnedLabel {
                key: l.key.into(),
                str_value: l.str.into(),
                num: l.num,
                num_unit: l.num_unit.into(),
            })
            .collect();

        TrackedAlloc {
            frames,
            labels,
            values: indices.build_values(sample.values),
        }
    }

    fn from_string_id_sample(
        sample: &api::StringIdSample,
        storage: &ManagedStringStorage,
        excluded_labels: &[Box<str>],
        indices: &ValueIndices,
    ) -> anyhow::Result<Self> {
        let frames = sample
            .locations
            .iter()
            .map(|loc| -> anyhow::Result<OwnedFrame> {
                Ok(OwnedFrame {
                    function_name: resolve_managed_string(storage, loc.function.name)?,
                    filename: resolve_managed_string(storage, loc.function.filename)?,
                    line: loc.line,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut labels = Vec::with_capacity(sample.labels.len());
        for l in &sample.labels {
            let key = resolve_managed_string(storage, l.key)?;
            if is_excluded(excluded_labels, &key) {
                continue;
            }
            labels.push(OwnedLabel {
                key,
                str_value: resolve_managed_string(storage, l.str)?,
                num: l.num,
                num_unit: resolve_managed_string(storage, l.num_unit)?,
            });
        }

        Ok(TrackedAlloc {
            frames,
            labels,
            values: indices.build_values(sample.values),
        })
    }
}

fn resolve_managed_string(
    storage: &ManagedStringStorage,
    id: ManagedStringId,
) -> anyhow::Result<Box<str>> {
    if id.value == 0 {
        return Ok(Box::from(""));
    }
    let rc_str = storage.get_string(id.value)?;
    Ok(Box::from(rc_str.as_ref()))
}
