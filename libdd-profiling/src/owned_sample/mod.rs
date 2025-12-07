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
//! use std::sync::Arc;
//!
//! let metadata = Arc::new(Metadata::new(vec![
//!     SampleType::CpuTime,
//!     SampleType::WallTime,
//! ], 64, true).unwrap());
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
//! sample.add_label(Label { key: "thread_name", str: "worker-1", num: 0, num_unit: "" });
//! sample.add_label(Label { key: "thread_id", str: "", num: 123, num_unit: "" });
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

/// Internal data structure that holds the arena and references into it.
/// This is a self-referential structure created using the ouroboros crate.
#[ouroboros::self_referencing]
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
    /// let indices = Arc::new(SampleTypeIndices::new(vec![
    ///     SampleType::Cpu,
    ///     SampleType::Wall,
    /// ]).unwrap());
    /// let sample = OwnedSample::new(indices);
    /// ```
    pub fn new(metadata: Arc<Metadata>) -> Self {
        let num_values = metadata.len();
        Self {
            inner: SampleInnerBuilder {
                arena: Bump::new(),
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
    /// # let indices = Arc::new(Metadata::new(vec![SampleType::CpuTime], 64, true).unwrap());
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
    /// If the number of locations has reached `max_frames`, the frame will be dropped
    /// and the dropped frame count will be incremented instead.
    pub fn add_location(&mut self, location: Location<'_>) {
        // Check if we've reached the max_frames limit
        let current_count = self.inner.borrow_locations().len();
        if current_count >= self.metadata.max_frames() {
            // Drop this frame and increment the counter
            self.dropped_frames += 1;
            return;
        }
        
        self.inner.with_mut(|fields| {
            // Allocate strings in the arena
            let filename_ref = fields.arena.alloc_str(location.mapping.filename);
            let build_id_ref = fields.arena.alloc_str(location.mapping.build_id);
            let name_ref = fields.arena.alloc_str(location.function.name);
            let system_name_ref = fields.arena.alloc_str(location.function.system_name);
            let func_filename_ref = fields.arena.alloc_str(location.function.filename);

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
        });
    }

    /// Add multiple locations to the sample.
    ///
    /// The locations' strings will be copied into the internal arena.
    pub fn add_locations(&mut self, locations: &[Location<'_>]) {
        for location in locations {
            self.add_location(*location);
        }
    }

    /// Add a label to the sample.
    ///
    /// The label's strings will be copied into the internal arena.
    pub fn add_label(&mut self, label: Label<'_>) {
        self.inner.with_mut(|fields| {
            let key_ref = fields.arena.alloc_str(label.key);
            let str_ref = fields.arena.alloc_str(label.str);
            let num_unit_ref = fields.arena.alloc_str(label.num_unit);

            let owned_label = Label {
                key: key_ref,
                str: str_ref,
                num: label.num,
                num_unit: num_unit_ref,
            };

            fields.labels.push(owned_label);
        });
    }

    /// Add multiple labels to the sample.
    ///
    /// The labels' strings will be copied into the internal arena.
    pub fn add_labels(&mut self, labels: &[Label<'_>]) {
        for label in labels {
            self.add_label(*label);
        }
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
    /// # let metadata = Arc::new(Metadata::new(vec![SampleType::CpuTime], 64, true).unwrap());
    /// # let mut sample = OwnedSample::new(metadata);
    /// sample.add_string_label(LabelKey::ThreadName, "worker-1");
    /// sample.add_string_label(LabelKey::ExceptionType, "ValueError");
    /// ```
    pub fn add_string_label(&mut self, key: LabelKey, value: &str) {
        self.add_label(Label {
            key: key.as_str(),
            str: value,
            num: 0,
            num_unit: "",
        });
    }

    /// Add a numeric label to the sample using a well-known label key.
    ///
    /// This is a convenience method for adding labels with numeric values.
    ///
    /// # Example
    /// ```no_run
    /// # use libdd_profiling::owned_sample::{OwnedSample, Metadata, SampleType, LabelKey};
    /// # use std::sync::Arc;
    /// # let metadata = Arc::new(Metadata::new(vec![SampleType::CpuTime], 64, true).unwrap());
    /// # let mut sample = OwnedSample::new(metadata);
    /// sample.add_num_label(LabelKey::ThreadId, 42);
    /// sample.add_num_label(LabelKey::SpanId, 12345);
    /// ```
    pub fn add_num_label(&mut self, key: LabelKey, value: i64) {
        self.add_label(Label {
            key: key.as_str(),
            str: "",
            num: value,
            num_unit: "",
        });
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
    /// # let metadata = Arc::new(Metadata::new(vec![SampleType::CpuTime, SampleType::WallTime], 64, true).unwrap());
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
    /// assert_eq!(sample.num_locations(), 0);
    /// assert_eq!(sample.num_labels(), 0);
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

    /// Get the number of locations in this sample.
    pub fn num_locations(&self) -> usize {
        self.inner.borrow_locations().len()
    }

    /// Get the number of labels in this sample.
    pub fn num_labels(&self) -> usize {
        self.inner.borrow_labels().len()
    }

    /// Get the number of frames that were dropped due to exceeding max_frames.
    pub fn dropped_frames(&self) -> usize {
        self.dropped_frames
    }

    /// Get a location by index.
    pub fn get_location(&self, index: usize) -> Option<Location<'_>> {
        self.inner.borrow_locations().get(index).copied()
    }

    /// Get a label by index.
    pub fn get_label(&self, index: usize) -> Option<Label<'_>> {
        self.inner.borrow_labels().get(index).copied()
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
    /// # let metadata = Arc::new(Metadata::new(vec![SampleType::CpuTime], 64, true).unwrap());
    /// # let mut profile = Profile::try_new(vec![], None).unwrap();
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

    /// Iterate over all locations.
    pub fn locations(&self) -> impl Iterator<Item = Location<'_>> + '_ {
        self.inner.borrow_locations().iter().copied()
    }

    /// Iterate over all labels.
    pub fn labels(&self) -> impl Iterator<Item = Label<'_>> + '_ {
        self.inner.borrow_labels().iter().copied()
    }
}

impl std::fmt::Debug for OwnedSample {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnedSample")
            .field("sample_types", &self.metadata.types())
            .field("num_locations", &self.num_locations())
            .field("num_labels", &self.num_labels())
            .field("values", &self.values())
            .finish()
    }
}

impl PartialEq for OwnedSample {
    fn eq(&self, other: &Self) -> bool {
        // Compare metadata configuration (pointer equality is fine since they're Arc)
        Arc::ptr_eq(&self.metadata, &other.metadata)
            && self.values() == other.values()
            && self.num_locations() == other.num_locations()
            && self.num_labels() == other.num_labels()
            && self.locations().zip(other.locations()).all(|(a, b)| a == b)
            && self.labels().zip(other.labels()).all(|(a, b)| a == b)
    }
}

impl Eq for OwnedSample {}

