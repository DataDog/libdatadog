// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Owned versions of profiling types that can be stored without lifetime constraints.
//!
//! These types use bumpalo arena allocation for strings - all strings within a sample are stored
//! in a bump allocator arena, and locations/labels reference them via the arena's lifetime.
//!
//! # Example
//!
//! ```no_run
//! use libdd_profiling::owned_sample::{OwnedSample, Metadata, SampleType};
//! use libdd_profiling::api::{Location, Mapping, Function, Label};
//! use std::sync::Arc;
//!
//! let metadata = Arc::new(Metadata::new(vec![
//!     SampleType::CpuTime,
//!     SampleType::WallTime,
//! ], 64, None, true).unwrap());
//!
//! let mut sample = OwnedSample::new(metadata);
//!
//! // Set values by type
//! sample.set_value(SampleType::CpuTime, 1000).unwrap();
//! sample.set_value(SampleType::WallTime, 2000).unwrap();
//!
//! // Add a location
//! sample.add_location(Location {
//!     mapping: Mapping {
//!         memory_start: 0x1000,
//!         memory_limit: 0x2000,
//!         file_offset: 0,
//!         filename: "libfoo.so",
//!         build_id: "abc123",
//!     },
//!     function: Function {
//!         name: "my_function",
//!         system_name: "_Z11my_functionv",
//!         filename: "foo.cpp",
//!     },
//!     address: 0x1234,
//!     line: 42,
//! });
//!
//! // Add labels
//! sample.add_label(Label { key: "thread_name", str: "worker-1", num: 0, num_unit: "" }).unwrap();
//! sample.add_label(Label { key: "thread_id", str: "", num: 123, num_unit: "" }).unwrap();
//! ```

use bumpalo::Bump;
use std::num::NonZeroI64;
use std::sync::Arc;
use anyhow::{self, Context};
use crate::api::{Function, Label, Location, Mapping, Sample};

mod label_key;
mod metadata;
mod pool;
mod sample_type;

#[cfg(test)]
mod tests;

pub use label_key::LabelKey;
pub use metadata::Metadata;
pub use sample_type::SampleType;
pub use pool::SamplePool;

/// Wrapper around bumpalo::AllocErr that implements std::error::Error
#[derive(Debug)]
pub struct AllocError(bumpalo::AllocErr);

impl std::fmt::Display for AllocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "arena allocation failed")
    }
}

impl std::error::Error for AllocError {}

impl From<bumpalo::AllocErr> for AllocError {
    fn from(err: bumpalo::AllocErr) -> Self {
        AllocError(err)
    }
}

/// Errors that can occur during owned sample operations.
#[derive(Debug, thiserror::Error)]
pub enum OwnedSampleError {
    /// Arena allocation failed (out of memory)
    #[error(transparent)]
    AllocationFailed(#[from] AllocError),
    
    /// Invalid sample type index
    #[error("invalid sample type index: {0}")]
    InvalidIndex(usize),
}

impl From<bumpalo::AllocErr> for OwnedSampleError {
    fn from(err: bumpalo::AllocErr) -> Self {
        OwnedSampleError::AllocationFailed(AllocError(err))
    }
}

/// Internal data structure that holds the arena and references into it.
/// This is a self-referential structure created using the ouroboros crate.
#[ouroboros::self_referencing]
#[derive(Debug)]
struct SampleInner {
    /// Bump arena where all strings are allocated
    arena: Bump,
    
    /// Locations with string references into the arena
    #[borrows(arena)]
    #[covariant]
    locations: Vec<Location<'this>>,
    
    /// Labels with string references into the arena
    #[borrows(arena)]
    #[covariant]
    labels: Vec<Label<'this>>,
}

/// An owned sample with arena-allocated strings.
///
/// All strings (in mappings, functions, labels) are stored in an internal bumpalo arena,
/// providing efficient memory usage and cache locality. The sample can be passed around
/// freely without lifetime constraints.
#[derive(Debug)]
pub struct OwnedSample {
    inner: SampleInner,
    values: Vec<i64>,
    metadata: Arc<Metadata>,
    endtime_ns: Option<NonZeroI64>,
    reverse_locations: bool,
    dropped_frames: usize,
}

impl OwnedSample {
    /// Creates a new empty sample with the given sample type indices.
    ///
    /// The values vector will be initialized with zeros, one for each sample type
    /// configured in the indices.
    ///
    /// # Example
    /// ```no_run
    /// # use libdd_profiling::owned_sample::{OwnedSample, Metadata, SampleType};
    /// # use std::sync::Arc;
    /// let metadata = Arc::new(Metadata::new(vec![
    ///     SampleType::CpuTime,
    ///     SampleType::WallTime,
    /// ], 64, None, true).unwrap());
    /// let sample = OwnedSample::new(metadata);
    /// ```
    pub fn new(metadata: Arc<Metadata>) -> Self {
        let num_values = metadata.len();
        let arena = Bump::new();
        arena.set_allocation_limit(metadata.arena_allocation_limit());
        Self {
            inner: SampleInnerBuilder {
                arena,
                locations_builder: |_| Vec::new(),
                labels_builder: |_| Vec::new(),
            }.build(),
            values: vec![0; num_values],
            metadata,
            endtime_ns: None,
            reverse_locations: false,
            dropped_frames: 0,
        }
    }

    /// Sets the value for the given sample type.
    ///
    /// # Errors
    ///
    /// Returns an error if the sample type is not configured.
    ///
    /// # Example
    /// ```no_run
    /// # use libdd_profiling::owned_sample::{OwnedSample, Metadata, SampleType};
    /// # use std::sync::Arc;
    /// # let indices = Arc::new(Metadata::new(vec![SampleType::CpuTime], 64, None, true).unwrap());
    /// let mut sample = OwnedSample::new(indices);
    /// sample.set_value(SampleType::CpuTime, 1000).unwrap();
    /// ```
    pub fn set_value(&mut self, sample_type: SampleType, value: i64) -> anyhow::Result<()> {
        let index = self.metadata.get_index(&sample_type)
            .with_context(|| format!("sample type {:?} not configured", sample_type))?;
        
        self.values[index] = value;
        
        Ok(())
    }

    /// Gets the value for the given sample type.
    ///
    /// # Errors
    ///
    /// Returns an error if the sample type is not configured.
    pub fn get_value(&self, sample_type: SampleType) -> anyhow::Result<i64> {
        let index = self.metadata.get_index(&sample_type)
            .with_context(|| format!("sample type {:?} not configured", sample_type))?;
        Ok(self.values[index])
    }

    /// Returns a reference to the sample metadata.
    pub fn metadata(&self) -> &Arc<Metadata> {
        &self.metadata
    }

    /// Returns whether locations should be reversed when converting to a Sample.
    pub fn is_reverse_locations(&self) -> bool {
        self.reverse_locations
    }

    /// Sets whether locations should be reversed when converting to a Sample.
    /// 
    /// When enabled, `as_sample()` will return locations in reverse order.
    pub fn set_reverse_locations(&mut self, reverse: bool) {
        self.reverse_locations = reverse;
    }

    /// Sets the end time of the sample in nanoseconds.
    /// 
    /// If `endtime_ns` is 0, the end time will be cleared (set to None).
    /// 
    /// Returns the timestamp that was passed in. If timeline is disabled,
    /// the value is not stored but is still returned.
    pub fn set_endtime_ns(&mut self, endtime_ns: i64) -> i64 {
        if self.metadata.is_timeline_enabled() {
            self.endtime_ns = NonZeroI64::new(endtime_ns);
        }
        endtime_ns
    }

    /// Sets the end time of the sample to the current time (now).
    ///
    /// On Unix platforms, this uses `CLOCK_MONOTONIC` for accurate timing and converts
    /// to epoch time. On other platforms, it uses the system clock directly.
    ///
    /// Returns the calculated timestamp. If timeline is disabled, the timestamp
    /// is calculated and returned but not stored in the sample.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - On Unix: system time is before UNIX_EPOCH or `clock_gettime(CLOCK_MONOTONIC)` fails
    /// - On non-Unix: system time is before UNIX_EPOCH
    #[cfg(unix)]
    pub fn set_endtime_ns_now(&mut self) -> anyhow::Result<i64> {
        // Get current monotonic time
        let ts = nix::time::clock_gettime(nix::time::ClockId::CLOCK_MONOTONIC)
            .context("failed to get current monotonic time")?;
        
        let monotonic_ns = ts.tv_sec() * 1_000_000_000 + ts.tv_nsec();
        
        // Convert to epoch time and set (set_endtime_from_monotonic_ns handles timeline check)
        self.set_endtime_from_monotonic_ns(monotonic_ns)
    }

    #[cfg(not(unix))]
    pub fn set_endtime_ns_now(&mut self) -> anyhow::Result<i64> {
        use std::time::SystemTime;
        
        let now_ns = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .context("system time is before UNIX_EPOCH")?
            .as_nanos() as i64;
        
        // set_endtime_ns returns the timestamp and handles timeline check
        Ok(self.set_endtime_ns(now_ns))
    }

    /// Returns the end time of the sample in nanoseconds, or None if not set.
    pub fn endtime_ns(&self) -> Option<NonZeroI64> {
        self.endtime_ns
    }

    /// Converts a monotonic timestamp (CLOCK_MONOTONIC) to epoch time and sets it as endtime_ns.
    ///
    /// Monotonic times have their epoch at system start, so they need an adjustment
    /// to the standard epoch. This function computes the offset once (on first call)
    /// and reuses it for all subsequent conversions.
    ///
    /// This uses `clock_gettime(CLOCK_MONOTONIC)` to determine the offset.
    ///
    /// Returns the converted epoch timestamp. If timeline is disabled, the timestamp
    /// is calculated and returned but not stored.
    ///
    /// # Arguments
    /// * `monotonic_ns` - Monotonic timestamp in nanoseconds since system boot
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - System time is before UNIX_EPOCH
    /// - `clock_gettime(CLOCK_MONOTONIC)` fails
    #[cfg(unix)]
    pub fn set_endtime_from_monotonic_ns(&mut self, monotonic_ns: i64) -> anyhow::Result<i64> {
        let offset = self.metadata.monotonic_to_epoch_offset();
        let endtime = monotonic_ns + offset;
        Ok(self.set_endtime_ns(endtime))
    }

    /// Add a location to the sample.
    ///
    /// The location's strings will be copied into the internal arena.
    /// If the number of locations has reached `max_frames` or arena allocation fails
    /// (e.g., allocation limit reached), the frame will be dropped and the dropped
    /// frame count will be incremented instead.
    pub fn add_location(&mut self, location: Location<'_>) {
        // Check if we've reached the max_frames limit
        let current_count = self.inner.borrow_locations().len();
        if current_count >= self.metadata.max_frames() {
            // Drop this frame and increment the counter
            self.dropped_frames += 1;
            return;
        }
        
        // Try to add the location, but if allocation fails, just drop the frame
        let result: Result<(), OwnedSampleError> = self.inner.with_mut(|fields| {
            // Allocate strings in the arena
            let filename_ref = fields.arena.try_alloc_str(location.mapping.filename)?;
            let build_id_ref = fields.arena.try_alloc_str(location.mapping.build_id)?;
            let name_ref = fields.arena.try_alloc_str(location.function.name)?;
            let system_name_ref = fields.arena.try_alloc_str(location.function.system_name)?;
            let func_filename_ref = fields.arena.try_alloc_str(location.function.filename)?;

            // Create location with references to arena strings
            let owned_location = Location {
                mapping: Mapping {
                    memory_start: location.mapping.memory_start,
                    memory_limit: location.mapping.memory_limit,
                    file_offset: location.mapping.file_offset,
                    filename: filename_ref,
                    build_id: build_id_ref,
                },
                function: Function {
                    name: name_ref,
                    system_name: system_name_ref,
                    filename: func_filename_ref,
                },
                address: location.address,
                line: location.line,
            };

            fields.locations.push(owned_location);
            Ok(())
        });
        
        // If allocation failed, drop the frame
        if result.is_err() {
            self.dropped_frames += 1;
        }
    }

    /// Add multiple locations to the sample.
    ///
    /// The locations' strings will be copied into the internal arena.
    /// Frames that exceed `max_frames` or cause allocation failures will be dropped.
    pub fn add_locations(&mut self, locations: &[Location<'_>]) {
        for location in locations {
            self.add_location(*location);
        }
    }

    /// Add a label to the sample.
    ///
    /// The label's strings will be copied into the internal arena.
    ///
    /// # Errors
    ///
    /// Returns an error if arena allocation fails (out of memory).
    pub fn add_label(&mut self, label: Label<'_>) -> Result<(), OwnedSampleError> {
        self.inner.with_mut(|fields| {
            let key_ref = fields.arena.try_alloc_str(label.key)?;
            let str_ref = fields.arena.try_alloc_str(label.str)?;
            let num_unit_ref = fields.arena.try_alloc_str(label.num_unit)?;

            let owned_label = Label {
                key: key_ref,
                str: str_ref,
                num: label.num,
                num_unit: num_unit_ref,
            };

            fields.labels.push(owned_label);
            Ok(())
        })
    }

    /// Add multiple labels to the sample.
    ///
    /// The labels' strings will be copied into the internal arena.
    ///
    /// # Errors
    ///
    /// Returns an error if arena allocation fails (out of memory).
    pub fn add_labels(&mut self, labels: &[Label<'_>]) -> Result<(), OwnedSampleError> {
        for label in labels {
            self.add_label(*label)?;
        }
        Ok(())
    }

    /// Add a string label to the sample using a well-known label key.
    ///
    /// This is a convenience method for adding labels with string values.
    /// The string will be copied into the internal arena.
    ///
    /// # Example
    /// ```no_run
    /// # use libdd_profiling::owned_sample::{OwnedSample, Metadata, SampleType, LabelKey};
    /// # use std::sync::Arc;
    /// # let metadata = Arc::new(Metadata::new(vec![SampleType::CpuTime], 64, None, true).unwrap());
    /// # let mut sample = OwnedSample::new(metadata);
    /// sample.add_string_label(LabelKey::ThreadName, "worker-1")?;
    /// sample.add_string_label(LabelKey::ExceptionType, "ValueError")?;
    /// # Ok::<(), libdd_profiling::owned_sample::OwnedSampleError>(())
    /// ```
    pub fn add_string_label(&mut self, key: LabelKey, value: &str) -> Result<(), OwnedSampleError> {
        self.add_label(Label {
            key: key.as_str(),
            str: value,
            num: 0,
            num_unit: "",
        })
    }

    /// Add a numeric label to the sample using a well-known label key.
    ///
    /// This is a convenience method for adding labels with numeric values.
    ///
    /// # Example
    /// ```no_run
    /// # use libdd_profiling::owned_sample::{OwnedSample, Metadata, SampleType, LabelKey};
    /// # use std::sync::Arc;
    /// # let metadata = Arc::new(Metadata::new(vec![SampleType::CpuTime], 64, None, true).unwrap());
    /// # let mut sample = OwnedSample::new(metadata);
    /// sample.add_num_label(LabelKey::ThreadId, 42)?;
    /// sample.add_num_label(LabelKey::SpanId, 12345)?;
    /// # Ok::<(), libdd_profiling::owned_sample::OwnedSampleError>(())
    /// ```
    pub fn add_num_label(&mut self, key: LabelKey, value: i64) -> Result<(), OwnedSampleError> {
        self.add_label(Label {
            key: key.as_str(),
            str: "",
            num: value,
            num_unit: "",
        })
    }

    /// Get the sample values.
    pub fn values(&self) -> &[i64] {
        &self.values
    }

    /// Get a mutable reference to the sample values.
    pub fn values_mut(&mut self) -> &mut [i64] {
        &mut self.values
    }

    /// Reset the sample, clearing all locations and labels, and zeroing all values.
    /// Reuses the arena and values vector allocations, avoiding reallocation overhead.
    ///
    /// # Example
    /// ```no_run
    /// # use libdd_profiling::owned_sample::{OwnedSample, Metadata, SampleType};
    /// # use libdd_profiling::api::{Location, Mapping, Function, Label};
    /// # use std::sync::Arc;
    /// # let metadata = Arc::new(Metadata::new(vec![SampleType::CpuTime, SampleType::WallTime], 64, None, true).unwrap());
    /// let mut sample = OwnedSample::new(metadata);
    /// sample.add_location(Location {
    ///     mapping: Mapping { memory_start: 0, memory_limit: 0, file_offset: 0, filename: "foo", build_id: "" },
    ///     function: Function { name: "bar", system_name: "", filename: "" },
    ///     address: 0,
    ///     line: 0,
    /// });
    /// sample.add_label(Label { key: "thread", str: "main", num: 0, num_unit: "" });
    /// 
    /// sample.reset();
    /// assert_eq!(sample.locations().len(), 0);
    /// assert_eq!(sample.labels().len(), 0);
    /// assert_eq!(sample.values(), &[0, 0]);  // Values are zeroed, not cleared
    /// ```
    pub fn reset(&mut self) {
        // Create a temporary empty inner to swap with
        let temp_inner = SampleInnerBuilder {
            arena: Bump::new(),
            locations_builder: |_| Vec::new(),
            labels_builder: |_| Vec::new(),
        }.build();
        
        // Replace self.inner with temp and extract the heads from the old one
        let old_inner = std::mem::replace(&mut self.inner, temp_inner);
        let mut heads = old_inner.into_heads();
        
        // Reset the arena - this reuses the allocation!
        heads.arena.reset();
        
        // Re-apply the allocation limit after reset
        heads.arena.set_allocation_limit(self.metadata.arena_allocation_limit());
        
        // Zero out all values but keep the vector length and capacity
        self.values.fill(0);
        
        self.endtime_ns = None;
        self.reverse_locations = false;
        self.dropped_frames = 0;
        
        // Rebuild with the reset arena
        self.inner = SampleInnerBuilder {
            arena: heads.arena,
            locations_builder: |_| Vec::new(),
            labels_builder: |_| Vec::new(),
        }.build();
    }

    /// Get the number of frames that were dropped due to exceeding max_frames.
    pub fn dropped_frames(&self) -> usize {
        self.dropped_frames
    }

    /// Get the number of bytes allocated in the internal arena.
    ///
    /// This includes all memory allocated for strings (location names, label keys, etc.)
    /// stored in this sample. Useful for tracking memory usage and pool optimization.
    pub fn allocated_bytes(&self) -> usize {
        self.inner.with(|fields| {
            fields.arena.allocated_bytes()
        })
    }

    /// Add this sample to a profile.
    ///
    /// If frames were dropped (exceeding `max_frames`), a pseudo-frame will be appended
    /// indicating how many frames were omitted. The profile will intern all strings, so
    /// no memory is leaked.
    ///
    /// # Example
    /// ```no_run
    /// # use libdd_profiling::owned_sample::{OwnedSample, Metadata, SampleType};
    /// # use libdd_profiling::internal::Profile;
    /// # use std::sync::Arc;
    /// # let metadata = Arc::new(Metadata::new(vec![SampleType::CpuTime], 64, None, true).unwrap());
    /// # let mut profile = Profile::try_new(&[], None).unwrap();
    /// let sample = OwnedSample::new(metadata);
    /// sample.add_to_profile(&mut profile).unwrap();
    /// ```
    pub fn add_to_profile(&self, profile: &mut crate::internal::Profile) -> anyhow::Result<()> {
        let mut locations = self.inner.borrow_locations().clone();
        
        // Reverse locations if the flag is set
        if self.reverse_locations {
            locations.reverse();
        }
        
        // If frames were dropped, add a pseudo-frame indicating how many
        let temp_name;
        if self.dropped_frames > 0 {
            let frame_word = if self.dropped_frames == 1 { "frame" } else { "frames" };
            temp_name = format!("<{} {} omitted>", self.dropped_frames, frame_word);
            
            // Create a pseudo-location for the dropped frames indicator
            let pseudo_location = Location {
                function: Function {
                    name: &temp_name,
                    ..Default::default()
                },
                ..Default::default()
            };
            
            locations.push(pseudo_location);
        }
        
        let sample = Sample {
            locations,
            values: &self.values,
            labels: self.inner.borrow_labels().clone(),
        };
        
        // Profile will intern the strings, including the temp_name if it was created
        profile.try_add_sample(sample, None)
    }

    /// Get a slice of all locations.
    pub fn locations(&self) -> &[Location<'_>] {
        self.inner.borrow_locations()
    }

    /// Get a slice of all labels.
    pub fn labels(&self) -> &[Label<'_>] {
        self.inner.borrow_labels()
    }
}
