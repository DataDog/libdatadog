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
//! use libdd_profiling::owned_sample::{OwnedSample, SampleTypeIndices, SampleType};
//! use std::sync::Arc;
//!
//! let indices = Arc::new(SampleTypeIndices::new(vec![
//!     SampleType::Cpu,
//!     SampleType::Wall,
//! ]).unwrap());
//!
//! let mut sample = OwnedSample::new(indices);
//!
//! // Set values by type
//! sample.set_value(SampleType::Cpu, 1000).unwrap();
//! sample.set_value(SampleType::Wall, 2000).unwrap();
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
use enum_map::{Enum, EnumMap};
use std::num::NonZeroI64;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use anyhow::{self, Context};
use crate::api::{Function, Label, Location, Mapping, Sample};

mod pool;

#[cfg(test)]
mod tests;

pub use pool::SamplePool;

/// Global flag to enable/disable timeline for all samples.
/// When disabled, time-setting methods become no-ops.
static TIMELINE_ENABLED: AtomicBool = AtomicBool::new(true);

/// Computes the offset between monotonic time (CLOCK_MONOTONIC) and epoch time.
/// This is computed once and cached in an atomic.
///
/// The offset allows converting monotonic timestamps (which start at system boot)
/// to epoch timestamps (which start at 1970-01-01).
///
/// # Errors
///
/// Returns an error if:
/// - System time is before UNIX_EPOCH
/// - `clock_gettime(CLOCK_MONOTONIC)` fails
#[cfg(unix)]
fn monotonic_to_epoch_offset() -> anyhow::Result<i64> {
    static OFFSET: AtomicI64 = AtomicI64::new(0);
    
    // Fast path: offset already computed
    let offset = OFFSET.load(Ordering::Relaxed);
    if offset != 0 {
        return Ok(offset);
    }
    
    // Slow path: compute the offset
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
    
    // Compute the difference (epoch_ns will be larger since we're after 1970)
    let computed_offset = epoch_ns - monotonic_ns;
    
    // Store it atomically (if another thread raced and stored it, that's fine)
    OFFSET.store(computed_offset, Ordering::Relaxed);
    
    Ok(computed_offset)
}


/// Types of profiling samples that can be collected.
///
/// Based on the sample types from [dd-trace-py](https://github.com/DataDog/dd-trace-py/blob/d239f91be2c4ca1ec2ded88263ed132e28fe031b/ddtrace/internal/datadog/profiling/dd_wrapper/include/types.hpp#L4).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Enum)]
pub enum SampleType {
    /// CPU time profiling
    Cpu,
    /// Wall clock time profiling
    Wall,
    /// Exception tracking
    Exception,
    /// Lock acquisition profiling
    LockAcquire,
    /// Lock release profiling
    LockRelease,
    /// Memory allocation profiling
    Allocation,
    /// Heap profiling
    Heap,
    /// GPU time profiling
    GpuTime,
    /// GPU memory profiling
    GpuMemory,
    /// GPU floating point operations profiling
    GpuFlops,
}

/// Maps sample types to their indices in a values array.
///
/// Each sample has a values array, and this struct tracks which index corresponds to
/// which sample type. This allows efficient O(1) indexing into the values array using
/// an `EnumMap` for lookups.
///
/// # Example
/// ```no_run
/// # use libdd_profiling::owned_sample::{SampleTypeIndices, SampleType};
/// let indices = SampleTypeIndices::new(vec![
///     SampleType::Cpu,
///     SampleType::Wall,
///     SampleType::Allocation,
/// ]).unwrap();
///
/// assert_eq!(indices.get_index(&SampleType::Cpu), Some(0));
/// assert_eq!(indices.get_index(&SampleType::Wall), Some(1));
/// assert_eq!(indices.get_index(&SampleType::Allocation), Some(2));
/// assert_eq!(indices.get_index(&SampleType::Heap), None);
/// assert_eq!(indices.len(), 3);
/// ```
#[derive(Clone, Debug)]
pub struct SampleTypeIndices {
    /// Ordered list of sample types
    sample_types: Vec<SampleType>,
    /// O(1) lookup map: sample type -> values array index
    /// None means the sample type is not configured
    type_to_index: EnumMap<SampleType, Option<usize>>,
}

impl SampleTypeIndices {
    /// Creates a new SampleTypeIndices with the given sample types.
    ///
    /// The order of sample types in the vector determines their index in the values array.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The sample types vector is empty
    /// - The same sample type appears more than once
    pub fn new(sample_types: Vec<SampleType>) -> anyhow::Result<Self> {
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

        Ok(Self {
            sample_types,
            type_to_index,
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
}

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
    indices: Arc<SampleTypeIndices>,
    endtime_ns: Option<NonZeroI64>,
}

impl OwnedSample {
    /// Creates a new empty sample with the given sample type indices.
    ///
    /// The values vector will be initialized with zeros, one for each sample type
    /// configured in the indices.
    ///
    /// # Example
    /// ```no_run
    /// # use libdd_profiling::owned_sample::{OwnedSample, SampleTypeIndices, SampleType};
    /// # use std::sync::Arc;
    /// let indices = Arc::new(SampleTypeIndices::new(vec![
    ///     SampleType::Cpu,
    ///     SampleType::Wall,
    /// ]).unwrap());
    /// let sample = OwnedSample::new(indices);
    /// ```
    pub fn new(indices: Arc<SampleTypeIndices>) -> Self {
        let num_values = indices.len();
        Self {
            inner: SampleInnerBuilder {
                arena: Bump::new(),
                locations_builder: |_| Vec::new(),
                labels_builder: |_| Vec::new(),
            }.build(),
            values: vec![0; num_values],
            indices,
            endtime_ns: None,
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
    /// # use libdd_profiling::owned_sample::{OwnedSample, SampleTypeIndices, SampleType};
    /// # use std::sync::Arc;
    /// # let indices = Arc::new(SampleTypeIndices::new(vec![SampleType::Cpu]).unwrap());
    /// let mut sample = OwnedSample::new(indices);
    /// sample.set_value(SampleType::Cpu, 1000).unwrap();
    /// ```
    pub fn set_value(&mut self, sample_type: SampleType, value: i64) -> anyhow::Result<()> {
        let index = self.indices.get_index(&sample_type)
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
        let index = self.indices.get_index(&sample_type)
            .with_context(|| format!("sample type {:?} not configured", sample_type))?;
        Ok(self.values[index])
    }

    /// Returns a reference to the sample type indices.
    pub fn indices(&self) -> &Arc<SampleTypeIndices> {
        &self.indices
    }

    /// Returns whether timeline is enabled globally for all samples.
    pub fn is_timeline_enabled() -> bool {
        TIMELINE_ENABLED.load(Ordering::Relaxed)
    }

    /// Sets whether timeline is enabled globally for all samples.
    /// 
    /// When timeline is disabled, time-setting methods become no-ops.
    pub fn set_timeline_enabled(enabled: bool) {
        TIMELINE_ENABLED.store(enabled, Ordering::Relaxed);
    }

    /// Sets the end time of the sample in nanoseconds.
    /// 
    /// If `endtime_ns` is 0, the end time will be cleared (set to None).
    /// 
    /// Returns the timestamp that was passed in. If timeline is disabled,
    /// the value is not stored but is still returned.
    pub fn set_endtime_ns(&mut self, endtime_ns: i64) -> i64 {
        if Self::is_timeline_enabled() {
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
        let offset = monotonic_to_epoch_offset()?;
        let endtime = monotonic_ns + offset;
        Ok(self.set_endtime_ns(endtime))
    }

    /// Add a location to the sample.
    ///
    /// The location's strings will be copied into the internal arena.
    pub fn add_location(&mut self, location: Location<'_>) {
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
    /// # use libdd_profiling::owned_sample::{OwnedSample, SampleTypeIndices, SampleType};
    /// # use libdd_profiling::api::{Location, Mapping, Function, Label};
    /// # use std::sync::Arc;
    /// # let indices = Arc::new(SampleTypeIndices::new(vec![SampleType::Cpu, SampleType::Wall]).unwrap());
    /// let mut sample = OwnedSample::new(indices);
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
        
        // Reset endtime_ns
        self.endtime_ns = None;
        
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

    /// Get a location by index.
    pub fn get_location(&self, index: usize) -> Option<Location<'_>> {
        self.inner.borrow_locations().get(index).copied()
    }

    /// Get a label by index.
    pub fn get_label(&self, index: usize) -> Option<Label<'_>> {
        self.inner.borrow_labels().get(index).copied()
    }

    /// Get a borrowed `Sample` view of this owned sample.
    /// The returned sample borrows from this OwnedSample.
    ///
    /// # Example
    /// ```no_run
    /// # use libdd_profiling::owned_sample::{OwnedSample, SampleTypeIndices, SampleType};
    /// # use std::sync::Arc;
    /// # let indices = Arc::new(SampleTypeIndices::new(vec![SampleType::Cpu]).unwrap());
    /// let sample = OwnedSample::new(indices);
    /// let borrowed = sample.as_sample();
    /// ```
    pub fn as_sample(&self) -> Sample<'_> {
        Sample {
            locations: self.inner.borrow_locations().clone(),
            values: &self.values,
            labels: self.inner.borrow_labels().clone(),
        }
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
            .field("sample_types", &self.indices.types())
            .field("num_locations", &self.num_locations())
            .field("num_labels", &self.num_labels())
            .field("values", &self.values())
            .finish()
    }
}

impl PartialEq for OwnedSample {
    fn eq(&self, other: &Self) -> bool {
        // Compare indices configuration (pointer equality is fine since they're Arc)
        Arc::ptr_eq(&self.indices, &other.indices)
            && self.values() == other.values()
            && self.num_locations() == other.num_locations()
            && self.num_labels() == other.num_labels()
            && self.locations().zip(other.locations()).all(|(a, b)| a == b)
            && self.labels().zip(other.labels()).all(|(a, b)| a == b)
    }
}

impl Eq for OwnedSample {}

