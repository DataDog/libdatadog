// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::api;
use crate::api::ManagedStringId;
use crate::api::SampleType;
use crate::collections::string_storage::ManagedStringStorage;
use std::collections::HashMap;
use std::hash::BuildHasherDefault;

/// Owned location data for heap-live tracking.
/// Stores copies of borrowed strings so tracked allocations survive across
/// profile resets.
pub(crate) struct OwnedMapping {
    pub memory_start: u64,
    pub memory_limit: u64,
    pub file_offset: u64,
    pub filename: Box<str>,
    pub build_id: Box<str>,
}

pub(crate) struct OwnedFunction {
    pub function_name: Box<str>,
    pub system_name: Box<str>,
    pub filename: Box<str>,
}

pub(crate) struct OwnedLocation {
    pub mapping: OwnedMapping,
    pub function: OwnedFunction,
    pub address: u64,
    pub line: i64,
}

/// Owned label for heap-live tracking.
pub(crate) struct OwnedLabel {
    pub key: Box<str>,
    pub str_value: Box<str>,
    pub num: i64,
    pub num_unit: Box<str>,
}

impl OwnedMapping {
    pub fn as_api_mapping(&self) -> api::Mapping<'_> {
        api::Mapping {
            memory_start: self.memory_start,
            memory_limit: self.memory_limit,
            file_offset: self.file_offset,
            filename: &self.filename,
            build_id: &self.build_id,
        }
    }
}

impl OwnedFunction {
    pub fn as_api_function(&self) -> api::Function<'_> {
        api::Function {
            name: &self.function_name,
            system_name: &self.system_name,
            filename: &self.filename,
        }
    }
}

impl OwnedLocation {
    pub fn as_api_location(&self) -> api::Location<'_> {
        api::Location {
            mapping: self.mapping.as_api_mapping(),
            function: self.function.as_api_function(),
            address: self.address,
            line: self.line,
        }
    }
}

impl OwnedLabel {
    pub fn as_api_label(&self) -> api::Label<'_> {
        api::Label {
            key: &self.key,
            str: &self.str_value,
            num: self.num,
            num_unit: &self.num_unit,
        }
    }
}

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
    fn for_sample_types(sample_types: &[SampleType]) -> anyhow::Result<Self> {
        fn index_of(sample_types: &[SampleType], target: SampleType) -> anyhow::Result<usize> {
            sample_types
                .iter()
                .position(|sample_type| *sample_type == target)
                .ok_or_else(|| {
                    anyhow::anyhow!("heap-live tracking requires sample type {target:?}")
                })
        }

        Ok(Self {
            alloc_size: index_of(sample_types, SampleType::AllocSize)?,
            heap_live_samples: index_of(sample_types, SampleType::HeapLiveSamples)?,
            heap_live_size: index_of(sample_types, SampleType::HeapLiveSize)?,
            num_values: sample_types.len(),
        })
    }

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

/// Tracks live heap allocations inside a Profile.
/// Stores owned copies of sample data (frames, labels, values) so tracked
/// allocations survive across profile resets. Injected into the Profile's
/// observations automatically before serialization via
/// `reset_and_return_previous()`.
pub(crate) struct HeapLiveState {
    pub tracked: HashMap<u64, TrackedAlloc, FxBuildHasher>,
    pub max_tracked: usize,
    pub excluded_labels: Vec<Box<str>>,
    indices: ValueIndices,
}

/// A single tracked live allocation with owned frame/label/values data.
pub(crate) struct TrackedAlloc {
    pub locations: Vec<OwnedLocation>,
    pub labels: Vec<OwnedLabel>,
    pub values: Vec<i64>,
}

impl HeapLiveState {
    /// Create a new heap-live tracker.
    pub fn new(
        max_tracked: usize,
        excluded_labels: &[&str],
        sample_types: &[SampleType],
    ) -> anyhow::Result<Self> {
        Ok(Self {
            tracked: HashMap::with_capacity_and_hasher(max_tracked, FxBuildHasher::default()),
            max_tracked,
            excluded_labels: excluded_labels.iter().map(|s| Box::from(*s)).collect(),
            indices: ValueIndices::for_sample_types(sample_types)?,
        })
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
        let locations = sample
            .locations
            .iter()
            .map(|loc| OwnedLocation {
                mapping: OwnedMapping {
                    memory_start: loc.mapping.memory_start,
                    memory_limit: loc.mapping.memory_limit,
                    file_offset: loc.mapping.file_offset,
                    filename: loc.mapping.filename.into(),
                    build_id: loc.mapping.build_id.into(),
                },
                function: OwnedFunction {
                    function_name: loc.function.name.into(),
                    system_name: loc.function.system_name.into(),
                    filename: loc.function.filename.into(),
                },
                address: loc.address,
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
            locations,
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
        let locations = sample
            .locations
            .iter()
            .map(|loc| -> anyhow::Result<OwnedLocation> {
                Ok(OwnedLocation {
                    mapping: OwnedMapping {
                        memory_start: loc.mapping.memory_start,
                        memory_limit: loc.mapping.memory_limit,
                        file_offset: loc.mapping.file_offset,
                        filename: resolve_managed_string(storage, loc.mapping.filename)?,
                        build_id: resolve_managed_string(storage, loc.mapping.build_id)?,
                    },
                    function: OwnedFunction {
                        function_name: resolve_managed_string(storage, loc.function.name)?,
                        system_name: resolve_managed_string(storage, loc.function.system_name)?,
                        filename: resolve_managed_string(storage, loc.function.filename)?,
                    },
                    address: loc.address,
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
            locations,
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
