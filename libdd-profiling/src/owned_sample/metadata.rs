// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Metadata for profiling samples, including sample type configuration.

use super::SampleType;
use anyhow::Context;
use enum_map::EnumMap;

/// Maps sample types to their indices in a values array.
///
/// Each sample has a values array, and this struct tracks which index corresponds to
/// which sample type. This allows efficient O(1) indexing into the values array using
/// an `EnumMap` for lookups.
///
/// # Example
/// ```no_run
/// # use libdd_profiling::owned_sample::{Metadata, SampleType};
/// let metadata = Metadata::new(vec![
///     SampleType::Cpu,
///     SampleType::Wall,
///     SampleType::Allocation,
/// ], 256, true).unwrap();
///
/// assert_eq!(metadata.get_index(&SampleType::Cpu), Some(0));
/// assert_eq!(metadata.get_index(&SampleType::Wall), Some(1));
/// assert_eq!(metadata.get_index(&SampleType::Allocation), Some(2));
/// assert_eq!(metadata.get_index(&SampleType::Heap), None);
/// assert_eq!(metadata.len(), 3);
/// assert_eq!(metadata.max_frames(), 256);
/// ```
#[derive(Clone, Debug)]
pub struct Metadata {
    /// Ordered list of sample types
    sample_types: Vec<SampleType>,
    /// O(1) lookup map: sample type -> values array index
    /// None means the sample type is not configured
    type_to_index: EnumMap<SampleType, Option<usize>>,
    /// Maximum number of stack frames to collect per sample
    max_frames: usize,
    /// Whether timeline is enabled for samples using this metadata.
    /// When disabled, time-setting methods become no-ops.
    timeline_enabled: bool,
    /// Offset between monotonic time and epoch time (Unix only).
    /// Allows converting CLOCK_MONOTONIC timestamps to epoch timestamps.
    #[cfg(unix)]
    monotonic_to_epoch_offset: i64,
}

impl Metadata {
    /// Creates a new Metadata with the given sample types, max frames, and timeline setting.
    ///
    /// The order of sample types in the vector determines their index in the values array.
    ///
    /// On Unix platforms, this also computes and caches the offset between monotonic time
    /// (CLOCK_MONOTONIC) and epoch time for efficient timestamp conversion.
    ///
    /// # Arguments
    ///
    /// * `sample_types` - The sample types to configure
    /// * `max_frames` - Maximum number of stack frames to collect per sample
    /// * `timeline_enabled` - Whether timeline should be enabled for samples using this metadata
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The sample types vector is empty
    /// - The same sample type appears more than once
    /// - (Unix only) System time is before UNIX_EPOCH
    /// - (Unix only) `clock_gettime(CLOCK_MONOTONIC)` fails
    pub fn new(sample_types: Vec<SampleType>, max_frames: usize, timeline_enabled: bool) -> anyhow::Result<Self> {
        anyhow::ensure!(!sample_types.is_empty(), "sample types cannot be empty");

        let mut type_to_index: EnumMap<SampleType, Option<usize>> = EnumMap::default();

        for (index, &sample_type) in sample_types.iter().enumerate() {
            anyhow::ensure!(
                type_to_index[sample_type].is_none(),
                "duplicate sample type: {:?}",
                sample_type
            );
            
            type_to_index[sample_type] = Some(index);
        }

        // Compute monotonic to epoch offset (Unix only)
        #[cfg(unix)]
        let monotonic_to_epoch_offset = {
            use std::time::SystemTime;
            
            // Get the current epoch time in nanoseconds
            let epoch_ns = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .context("system time is before UNIX_EPOCH")?
                .as_nanos() as i64;
            
            // Get the current monotonic time using clock_gettime (safe wrapper from nix crate)
            let ts = nix::time::clock_gettime(nix::time::ClockId::CLOCK_MONOTONIC)
                .context("failed to get monotonic time from CLOCK_MONOTONIC")?;
            
            let monotonic_ns = ts.tv_sec() * 1_000_000_000 + ts.tv_nsec();
            
            // Compute the offset (epoch_ns will be larger since we're after 1970)
            epoch_ns - monotonic_ns
        };

        Ok(Self {
            sample_types,
            type_to_index,
            max_frames,
            timeline_enabled,
            #[cfg(unix)]
            monotonic_to_epoch_offset,
        })
    }

    /// Returns the index for the given sample type, or None if not configured.
    pub fn get_index(&self, sample_type: &SampleType) -> Option<usize> {
        self.type_to_index[*sample_type]
    }

    /// Returns the sample type at the given index, or None if out of bounds.
    pub fn get_type(&self, index: usize) -> Option<SampleType> {
        self.sample_types.get(index).copied()
    }

    /// Returns the number of configured sample types.
    pub fn len(&self) -> usize {
        self.sample_types.len()
    }

    /// Returns true if no sample types are configured.
    pub fn is_empty(&self) -> bool {
        self.sample_types.is_empty()
    }

    /// Returns an iterator over the sample types in order.
    pub fn iter(&self) -> impl Iterator<Item = &SampleType> {
        self.sample_types.iter()
    }

    /// Returns a slice of all configured sample types in order.
    pub fn types(&self) -> &[SampleType] {
        &self.sample_types
    }

    /// Returns the maximum number of stack frames to collect per sample.
    pub fn max_frames(&self) -> usize {
        self.max_frames
    }

    /// Returns whether timeline is enabled for samples using this metadata.
    pub fn is_timeline_enabled(&self) -> bool {
        self.timeline_enabled
    }

    /// Returns the offset between monotonic time and epoch time (Unix only).
    ///
    /// This offset is computed once during construction and allows converting
    /// CLOCK_MONOTONIC timestamps to epoch timestamps.
    #[cfg(unix)]
    pub fn monotonic_to_epoch_offset(&self) -> i64 {
        self.monotonic_to_epoch_offset
    }
}

