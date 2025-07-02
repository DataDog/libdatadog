// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::{LabelsSet, Range};
use datadog_alloc::Box;
use datadog_profiling::{
    collections::SliceSet,
    profiles::{ProfileError, ProfileVoidResult, SliceId},
};
use datadog_profiling_protobuf::{Label, Record, ValueType, NO_OPT_ZERO};
use ddcommon_ffi::Slice;
use std::{mem, ptr};

type HashMap<K, V> = std::collections::HashMap<
    K,
    V,
    std::hash::BuildHasherDefault<rustc_hash::FxHasher>,
>;

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Sample<'a> {
    stack_trace_id: SliceId,
    values: Slice<'a, i64>,
    /// Don't use a timestamp label, use the `timestamp` field for that.
    labels: SliceId,
    /// Use 0 for no timestamp, we can do `Option<NonZeroI64>` one day. This
    /// timestamp is nanoseconds since the Unix epoch.
    timestamp: i64,
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

    pub fn new(types: &[ValueType]) -> Result<SampleManager, ProfileError> {
        let mut boxed = Box::try_new_uninit_slice(types.len())?;
        // SAFETY: &[MaybeUninit<T>] and &[T] have the same layout.
        // This is what unstable `MaybeUninit::copy_from_slice` does.
        let slice: &[mem::MaybeUninit<ValueType>] =
            unsafe { mem::transmute(types) };
        boxed.copy_from_slice(slice);
        Ok(Self {
            // SAFETY: just initialized from the provided types.
            types: unsafe { boxed.assume_init() },
            timestamped_samples: vec![],
            aggregated_samples: Default::default(),
            value_storage: vec![],
        })
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
    fn store_values(
        value_storage: &mut Vec<i64>,
        values: &[i64],
    ) -> Result<Range, ProfileError> {
        let Ok(additional) = u32::try_from(values.len()) else {
            // Why are you providing so many values at once?
            return Err(ProfileError::InvalidInput);
        };
        // SAFETY: invariant is preserved at all times.
        let start =
            unsafe { u32::try_from(value_storage.len()).unwrap_unchecked() };
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
    ///  1. Returns `InvalidInput` if `values.len()` doesn't fit in u32, or if
    ///     the number of samples doesn't match the number of sample types.
    ///  2. Returns `StorageFull` if underlying collection sizes don't fit
    ///     into `u32`.
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

    fn add_aggregate_sample(
        &mut self,
        sample: Sample,
    ) -> Result<(), ProfileError> {
        // Check some common slice invariants.
        let Some(values) = sample.values.try_as_slice() else {
            return Err(ProfileError::InvalidInput);
        };

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

    fn add_timestamped_sample(
        &mut self,
        sample: Sample,
    ) -> Result<(), ProfileError> {
        // Check some common slice invariants.
        let Some(values) = sample.values.try_as_slice() else {
            return Err(ProfileError::InvalidInput);
        };

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
    ) -> impl Iterator<Item = (datadog_profiling_protobuf::Sample<'a>, i64)>
    {
        self.aggregated_samples.iter().filter_map(|(sample, range)| {
            let range = core::ops::Range::<usize>::from(range);
            match self.value_storage.get(range) {
                Some(value) => {
                    let stack_trace = stack_trace_store
                        .lookup(sample.stack_trace_id.into())?;
                    let labels = labels_set.lookup(sample.labels.into())?;

                    Some((
                        datadog_profiling_protobuf::Sample {
                            location_ids: stack_trace.into(),
                            values: value.into(),
                            labels: unsafe {
                                mem::transmute::<
                                    &[Label],
                                    &[Record<Label, 3, NO_OPT_ZERO>],
                                >(labels)
                            },
                        },
                        0, // Aggregated samples have no timestamp
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
    ) -> impl Iterator<Item = (datadog_profiling_protobuf::Sample<'a>, i64)>
    {
        self.timestamped_samples.iter().filter_map(|sample| {
            let stack_trace =
                stack_trace_set.lookup(sample.stack_trace_id.into())?;
            let range = core::ops::Range::<usize>::from(sample.values);
            let labels = labels_set.lookup(sample.labels.into())?;
            Some((
                datadog_profiling_protobuf::Sample {
                    location_ids: stack_trace.into(),
                    values: unsafe {
                        self.value_storage.get_unchecked(range).into()
                    },
                    labels: unsafe {
                        mem::transmute::<
                            &[Label],
                            &[Record<Label, 3, NO_OPT_ZERO>],
                        >(labels)
                    },
                },
                sample.timestamp,
            ))
        })
    }
}

#[repr(C)]
pub enum SampleManagerNewResult {
    Ok(*mut SampleManager),
    Err(ProfileError),
}

#[no_mangle]
#[must_use]
pub extern "C" fn ddog_prof_SampleManager_new(
    sample_types: Slice<ValueType>,
) -> SampleManagerNewResult {
    let Some(sample_types) = sample_types.try_as_slice() else {
        return SampleManagerNewResult::Err(ProfileError::InvalidInput);
    };

    let sample_manager = match SampleManager::new(sample_types) {
        Ok(ok) => ok,
        Err(err) => return SampleManagerNewResult::Err(err),
    };

    let Ok(boxed) = Box::try_new(sample_manager) else {
        return SampleManagerNewResult::Err(ProfileError::OutOfMemory);
    };
    SampleManagerNewResult::Ok(Box::into_raw(boxed))
}

/// # Safety
///
/// The `sample_manager` must be a valid pointer to a `SampleManager`.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_prof_SampleManager_add_sample(
    sample_manager: Option<&mut SampleManager>,
    sample: Sample,
) -> ProfileVoidResult {
    let Some(sample_manager) = sample_manager else {
        return ProfileVoidResult::Err(ProfileError::InvalidInput);
    };

    ProfileVoidResult::from(sample_manager.add_sample(sample))
}

/// # Safety
///
/// The `m` must be a valid pointer to a pointer to a `SampleManager`.
/// `*m` may be null (this function handles null gracefully).
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_SampleManager_drop(
    m: *mut *mut SampleManager,
) {
    if let Some(ptr) = m.as_mut() {
        let inner_ptr = *ptr;
        if !inner_ptr.is_null() {
            drop(Box::from_raw(inner_ptr));
            *ptr = ptr::null_mut();
        }
    }
}
