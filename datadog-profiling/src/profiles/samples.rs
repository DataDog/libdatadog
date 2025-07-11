// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{
    collections::{Range, SliceSet},
    profiles::{LabelsSet, ProfileError},
};
use datadog_alloc::Box;
use datadog_profiling_protobuf::{Label, Record, ValueType, NO_OPT_ZERO};
use std::mem;

type HashMap<K, V> =
    std::collections::HashMap<K, V, std::hash::BuildHasherDefault<rustc_hash::FxHasher>>;

// Re-export SliceId from collections
pub use crate::collections::SliceId;

#[derive(Copy, Clone, Debug)]
pub struct Sample<'a> {
    pub stack_trace_id: SliceId,
    pub values: &'a [i64],
    /// Don't use a timestamp label, use the `timestamp` field for that.
    pub labels: SliceId,
    /// Use 0 for no timestamp, we can do `Option<NonZeroI64>` one day. This
    /// timestamp is nanoseconds since the Unix epoch.
    pub timestamp: i64,
}

pub struct SampleManager {
    /// The sample types for this profile.
    types: Box<[ValueType]>,
    /// The timestamped samples, which are not aggregated.
    timestamped_samples: Vec<TimestampedSample>,
    /// The non-timestamped samples, which we aggregate by the stack trace id
    /// and labels.
    aggregated_samples: HashMap<SampleKey, Range>,
    /// The [`Ranges`] from `timestamped_samples` and `aggregated_samples` are
    /// ranges into this vec.
    value_storage: Vec<i64>,
}

impl SampleManager {
    /// Returns the sample types for this sample manager.
    #[inline]
    pub fn types(&self) -> &[ValueType] {
        &self.types
    }

    /// Creates a new SampleManager from an exact-size iterator of ValueTypes.
    /// This avoids intermediate allocations by building the Box directly.
    pub fn new<I>(types: I) -> Result<SampleManager, ProfileError>
    where
        I: IntoIterator<Item = Result<ValueType, ProfileError>>,
        <I as IntoIterator>::IntoIter: ExactSizeIterator,
    {
        let iter = types.into_iter();
        let len = iter.len();

        let mut boxed = Box::try_new_uninit_slice(len)?;

        for (slot, value_type) in boxed.iter_mut().zip(iter) {
            slot.write(value_type?);
        }

        Ok(Self {
            // SAFETY: we just initialized all elements above (empty slice is trivially initialized)
            types: unsafe { boxed.assume_init() },
            timestamped_samples: vec![],
            aggregated_samples: Default::default(),
            value_storage: vec![],
        })
    }

    /// Clears all samples while preserving the sample types.
    /// This is more efficient than dropping and recreating the SampleManager.
    pub fn clear(&mut self) {
        self.timestamped_samples.clear();
        self.aggregated_samples.clear();
        self.value_storage.clear();
    }
}

#[derive(Copy, Clone, Debug)]
pub struct TimestampedSample {
    stack_trace_id: SliceId,
    values: Range,
    labels: SliceId,
    timestamp: i64,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct SampleKey {
    stack_trace_id: SliceId,
    labels: SliceId,
}

impl SampleManager {
    /// Tries to store the values, and returns a [`Range`] where it was added.
    fn store_values(value_storage: &mut Vec<i64>, values: &[i64]) -> Result<Range, ProfileError> {
        let Ok(additional) = u32::try_from(values.len()) else {
            // Why are you providing so many values at once?
            return Err(ProfileError::InvalidInput);
        };
        // SAFETY: invariant is preserved at all times.
        let start = unsafe { u32::try_from(value_storage.len()).unwrap_unchecked() };
        let Some(end) = start.checked_add(additional) else {
            return Err(ProfileError::StorageFull);
        };
        value_storage.try_reserve(additional as usize)?;
        value_storage.extend_from_slice(values);
        Ok(Range { start, end })
    }

    /// Tries to add the sample.
    ///
    /// # Errors
    ///
    ///  1. Returns `InvalidInput` if `values.len()` doesn't fit in u32, or if the number of samples
    ///     doesn't match the number of sample types.
    ///  2. Returns `StorageFull` if underlying collection sizes don't fit into `u32`.
    ///  3. Returns `OutOfMemory` if a collection fails to allocate memory.
    pub fn add_sample(&mut self, sample: Sample) -> Result<(), ProfileError> {
        if sample.values.len() != self.types.len() {
            return Err(ProfileError::InvalidInput);
        }
        if sample.timestamp != 0 {
            self.add_timestamped_sample(sample)
        } else {
            self.add_aggregate_sample(sample)
        }
    }

    fn add_aggregate_sample(&mut self, sample: Sample) -> Result<(), ProfileError> {
        let values = sample.values;

        let aggregated_samples = &mut self.aggregated_samples;
        let value_storage = &mut self.value_storage;

        // Try to get an existing payload.
        let key = SampleKey {
            stack_trace_id: sample.stack_trace_id,
            labels: sample.labels,
        };
        if let Some(range) = aggregated_samples.get(&key) {
            // If the key exists, sum the prev and new values element-wise.
            let range: core::ops::Range<usize> = range.into();
            let prev_values = unsafe { value_storage.get_unchecked_mut(range) };
            for (prev, new) in prev_values.iter_mut().zip(values) {
                *prev += *new;
            }
        } else {
            // Didn't exist, try to reserve space and add new key + payload.
            aggregated_samples.try_reserve(1)?;
            let payload = Self::store_values(value_storage, values)?;
            // Will not allocate, space was reserved above with try_reserve.
            aggregated_samples.insert(key, payload);
        }
        Ok(())
    }

    fn add_timestamped_sample(&mut self, sample: Sample) -> Result<(), ProfileError> {
        let values = sample.values;

        self.timestamped_samples.try_reserve(1)?;
        let values = Self::store_values(&mut self.value_storage, values)?;
        let timestamped_sample = TimestampedSample {
            stack_trace_id: sample.stack_trace_id,
            values,
            labels: sample.labels,
            timestamp: sample.timestamp,
        };
        // Will not allocate, space was reserved above with try_reserve.
        self.timestamped_samples.push(timestamped_sample);
        Ok(())
    }

    pub fn aggregated_samples<'a>(
        &'a self,
        labels_set: &'a LabelsSet,
        stack_trace_store: &'a SliceSet<u64>,
    ) -> impl Iterator<Item = (datadog_profiling_protobuf::Sample<'a>, i64)> {
        self.aggregated_samples
            .iter()
            .filter_map(|(sample, range)| {
                let range = core::ops::Range::<usize>::from(range);
                match self.value_storage.get(range) {
                    Some(value) => {
                        let stack_trace = stack_trace_store.lookup(sample.stack_trace_id.into())?;
                        let labels = labels_set.lookup(sample.labels.into())?;

                        Some((
                            datadog_profiling_protobuf::Sample {
                                location_ids: stack_trace.into(),
                                values: value.into(),
                                labels: unsafe {
                                    mem::transmute::<&[Label], &[Record<Label, 3, NO_OPT_ZERO>]>(
                                        labels,
                                    )
                                },
                            },
                            0, // Timestamp is 0 for aggregated samples
                        ))
                    }
                    None => None,
                }
            })
    }

    pub fn timestamped_samples<'a>(
        &'a self,
        labels_set: &'a LabelsSet,
        stack_trace_set: &'a SliceSet<u64>,
    ) -> impl Iterator<Item = (datadog_profiling_protobuf::Sample<'a>, i64)> {
        self.timestamped_samples.iter().filter_map(|sample| {
            let range = core::ops::Range::<usize>::from(&sample.values);
            match self.value_storage.get(range) {
                Some(value) => {
                    let stack_trace = stack_trace_set.lookup(sample.stack_trace_id.into())?;
                    let labels = labels_set.lookup(sample.labels.into())?;

                    Some((
                        datadog_profiling_protobuf::Sample {
                            location_ids: stack_trace.into(),
                            values: value.into(),
                            labels: unsafe {
                                mem::transmute::<&[Label], &[Record<Label, 3, NO_OPT_ZERO>]>(labels)
                            },
                        },
                        sample.timestamp,
                    ))
                }
                None => None,
            }
        })
    }
}
