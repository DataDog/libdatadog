// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profile_handle::ProfileHandle;
use crate::profiles::{
    ddog_prof_SampleBuilder_new, ddog_prof_SampleBuilder_value, SampleBuilder,
};
use crate::{ensure_non_null_out_parameter, ArcHandle, ProfileStatus};
use core::mem;
use datadog_profiling::profiles::datatypes::{
    Profile, ProfilesDictionary, ScratchPad, ValueType, MAX_SAMPLE_TYPES,
};
use datadog_profiling::profiles::ProfileError;
use ddcommon_ffi::Slice;
use std::ops::Range;

/// Allocates a new `Profile` and writes a handle to `handle`.
///
/// # Safety
///
/// - `handle` must be non-null and valid for writes of `ProfileHandle<_>`.
/// - The written handle must be dropped via the matching drop function;
///   see [`ddog_prof_Profile_drop`] for more details.
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Profile_new(
    handle: *mut ProfileHandle<Profile>,
) -> ProfileStatus {
    ensure_non_null_out_parameter!(handle);
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let h = ProfileHandle::try_new(Profile::default())?;
        unsafe { handle.write(h) };
        Ok(())
    }())
}

/// Adds a sample type to a profile.
///
/// # Safety
///
/// - `handle` must refer to a live `Profile` and is treated as a unique
///   mutable reference for the duration of the call (no aliasing mutations).
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Profile_add_sample_type(
    mut handle: ProfileHandle<Profile>,
    vt: ValueType,
) -> ProfileStatus {
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let prof = unsafe { handle.as_inner_mut()? };
        prof.try_add_sample_type(vt)
    }())
}

/// Sets the period and adds its `ValueType` to the profile.
///
/// # Safety
///
/// - `handle` must refer to a live `Profile` and is treated as a unique
///   mutable reference for the duration of the call (no aliasing mutations).
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Profile_add_period(
    mut handle: ProfileHandle<Profile>,
    period: i64,
    vt: ValueType,
) -> ProfileStatus {
    ProfileStatus::from(|| -> Result<(), ProfileError> {
        let prof = unsafe { handle.as_inner_mut()? };
        prof.add_period(period, vt);
        Ok(())
    }())
}

/// Drops the contents of the profile handle, leaving an empty handle behind.
///
/// # Safety
///
/// Pointer must point to a valid profile handle if not null.
///
/// The underlying resource must only be dropped through a single handle, and
/// once the underlying profile has been dropped, all other handles are invalid
/// and should be discarded without dropping them.
///
/// However, this function is safe to call multiple times on the _same handle_.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_Profile_drop(
    handle: *mut ProfileHandle<Profile>,
) {
    if let Some(h) = handle.as_mut() {
        drop(h.take());
    }
}

/// An adapter from the offset-based pprof format to the separate profiles
/// format that sort of mirrors the otel format.
///
/// Don't mutate this directly. Its definition is available for FFI layout
/// reasons and you can use it to iterate over the profiles, but it should
/// only be created and modified through the profile adapter FFI functions.
/// Otherwise, you could corrupt the adapter and crash.
#[derive(Default)]
#[repr(C)]
pub struct ProfileAdapter {
    dictionary: ArcHandle<ProfilesDictionary>,
    mappings: ddcommon_ffi::vec::Vec<ProfileAdapterMapping>,
}

#[repr(C)]
pub struct ProfileAdapterMapping {
    profile: ProfileHandle<Profile>,
    /// This is the range in the sample types/values array in the legacy API
    /// that corresponds to this mapping.
    range: Range<usize>,
}

impl Drop for ProfileAdapter {
    fn drop(&mut self) {
        let mut mappings = mem::take(&mut self.mappings).into_std();
        for mut mapping in mappings.drain(..) {
            drop(unsafe { mapping.profile.take() })
        }

        self.dictionary.drop_resource();
    }
}

/// Creates an adapter that maps the legacy offset-based sample model
/// (one flat list of sample types/values) into multiple Profiles, each with
/// 1–2 sample types.
///
/// Inputs must satisfy:
/// - `value_types.len() == groupings.len()`
/// - `value_types.len() > 0 && groupings.len() > 0`
/// - `groupings` is a sequence of contiguous "runs". Each run defines one
///   Profile and must have length 1 or 2. These groupings all define the same
///   runs:
///     - `[ 0,  0,  1,  0,  0]`
///     - `[ 0,  0,  1,  2,  2]`
///     - `[13, 13,  0,  5,  5]`
///
/// On success, a handle to the new `ProfileAdapter` is written to `out`. Drop
/// it with `ddog_prof_ProfileAdapter_drop`.
///
/// Here is a partial C example using some PHP profiles.
///
/// ```c
/// ddog_prof_ProfilesDictionaryHandle dictionary = /* ... */;
///
/// // Assume these ValueType entries were populated using your string table
/// // (type/unit ids). Order corresponds to the legacy offsets:
/// //   [wall-time, wall-samples, cpu-time, alloc-bytes, alloc-count]
/// ddog_prof_ValueType value_types[5] = {
///     wall_time, wall_samples, cpu_time, alloc_bytes, alloc_count
/// };
/// int64_t groupings[5] = { 0, 0, 1, 2, 2 };
///
/// ddog_prof_ProfileAdapter adapter;
/// ddog_prof_ProfileStatus st = ddog_prof_ProfileAdapter_new(
///     &adapter,
///     dictionary,
///     (ddog_Slice_ValueType){ .ptr = value_types, .len = 5 },
///     (ddog_Slice_I64){ .ptr = groupings, .len = 5 }
/// );
/// if (st.flags != 0) {
///     // handle error, then:
///     ddog_prof_Status_drop(&st)
/// }
///
/// // ...later...
///
/// // Allocation sample was taken.
/// int64_t values[5] = { 0, 0, 0, 128, 1 };
/// ddog_Slice_I64 ffi_slice = { .ptr = values, len = 5 };
///
/// ddog_prof_SampleBuilderHandle sample_builder_handle;
/// st = ddog_prof_ProfileAdapter_add_sample(
///     &sample_builder_handle,
///     adapter,
///     profile_index,
///     ffi_slice,
///     scratchpad,
/// );
///
/// // check st, then you can use SampleBuilder methods
/// // to add timestamps, links, etc.
///
/// // then add it to the profile:
/// st = ddog_prof_SampleBuilder_build(
///     &sample_builder_handle,
/// );
///
/// // ...later...
/// ddog_prof_ProfileAdapter_add_proportional_upscaling(adapter, 1, vec[ , ], ASSUME_UTF8)
/// ddog_prof_ProfileAdapter_add_upscaling_proportional(
///     index: usize,
///
/// )
/// PprofBuilderHandle pprof_builder;
/// upscalings[];
/// for i in n_profiles {
///     // if no upscaling
///     ddog_prof_PprofBuilder_add_profile(pprof_builder, adapter.profiles.ptr[i])
///
///     // if propoertional
///     ddog_prof_PprofBuilder_add_profile_proportional(pprof_builder, adapter.profiles.ptr[i], ...)
///     ddog_prof_PprofBuilder_add_profile_poisson(pprof_builder, adapter.profiles.ptr[i], ...)
/// }
///
///
///
/// // order of these doesn't matter, the adapter keeps a refcount
/// // alive on the dictionary.
/// ddog_prof_ProfilesDictionaryHandle_drop(&dictionary);
/// ddog_prof_ProfileAdapter_drop(&adapter);
/// ```
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfileAdapter_new(
    out: *mut ProfileAdapter,
    dictionary: ArcHandle<ProfilesDictionary>,
    value_types: Slice<'_, ValueType>,
    groupings: Slice<'_, i64>,
) -> ProfileStatus {
    // Ensure slices and inputs are valid.
    if out.is_null() {
        return ProfileStatus::from(c"invalid input: argument out to ddog_prof_ProfileAdapter_new was null");
    }
    let Ok(value_types) = value_types.try_as_slice() else {
        return ProfileStatus::from(c"invalid input: argument value_types to ddog_prof_ProfileAdapter_new failed to convert to a Rust slice");
    };
    let Ok(groupings) = groupings.try_as_slice() else {
        return ProfileStatus::from(c"invalid input: argument groupings to ddog_prof_ProfileAdapter_new failed to convert to a Rust slice");
    };

    // Ensure the value_types and groupings have the same length.
    if value_types.len() != groupings.len() {
        return ProfileStatus::from(c"invalid input: arguments value_types and groupings to ddog_prof_ProfileAdapter_new had mismatched lengths");
    }
    // Ensure the slices have at least 1 element.
    if value_types.is_empty() {
        return ProfileStatus::from(c"invalid input: arguments value_types and groupings to ddog_prof_ProfileAdapter_new must not be empty");
    }

    let Ok(dictionary) = dictionary.try_clone() else {
        return ProfileStatus::from(c"reference count overflow: profile adapter could not clone the profiles dictionary");
    };

    // Count runs and validate max run length.
    let (n_runs, longest_run) = count_runs_and_longest_run(groupings);
    if longest_run > MAX_SAMPLE_TYPES {
        return ProfileStatus::from(
            c"invalid input: groupings must appear in runs of length at most 2",
        );
    }

    // Build mapping of profiles (one per contiguous run).
    let mut mappings = ddcommon_ffi::vec::Vec::new();
    mappings.try_reserve_exact(n_runs).unwrap();

    for run in RunsIter::new(groupings) {
        // Create a profile for this run
        let mut mapping = ProfileAdapterMapping {
            profile: Default::default(),
            range: Default::default(),
        };
        let result = ddog_prof_Profile_new(&mut mapping.profile);
        if result.flags != 0 {
            return result;
        }
        mapping.range = run.clone();
        let profile = mapping.profile;
        mappings.push(mapping);
        // Add sample types for the run. The run length was previously
        // validated to be <= MAX_SAMPLE_TYPES.
        for value_idx in run {
            let status = ddog_prof_Profile_add_sample_type(
                profile,
                value_types[value_idx],
            );
            if status.flags != 0 {
                return status;
            }
        }
    }

    unsafe { out.write(ProfileAdapter { dictionary, mappings }) };
    ProfileStatus::OK
}

fn count_runs_and_longest_run(groupings: &[i64]) -> (usize, usize) {
    // Do it all in one pass.
    RunsIter::new(groupings).fold((0, 0), |(n_runs, longest), run| {
        (n_runs + 1, longest.max(run.len()))
    })
}

/// Maps the non-zero values to a profile, and returns using out parameters
/// the profile handle it matches, and a sample builder handle. The values
/// have already been added to the sample builder; the caller still needs to
/// add stack, timestamp, link, etc to the  sample builder and then build it
/// into the profile.
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfileAdapter_build_sample(
    sample_builder: *mut ProfileHandle<SampleBuilder>,
    adapter: &ProfileAdapter,
    profile_grouping: usize,
    values: Slice<'_, i64>,
    scratchpad: ArcHandle<ScratchPad>,
) -> ProfileStatus {
    assert!(!sample_builder.is_null());
    assert!(profile_grouping < adapter.mappings.len());
    if adapter.mappings.is_empty() {
        return ProfileStatus::from(c"invalid input: ddog_prof_ProfileAdapter_build_sample was called on an empty adapter");
    }
    let values = values.try_as_slice().unwrap();

    let Some(mapping) = adapter.mappings.get(profile_grouping) else {
        return ProfileStatus::from(c"invalid input: grouping passed to ddog_prof_ProfileAdapter_build_sample was out of range");
    };

    let mut builder = ProfileHandle::default();
    let status =
        ddog_prof_SampleBuilder_new(&mut builder, mapping.profile, scratchpad);
    if status.flags != 0 {
        return status;
    }
    for val in values[mapping.range.clone()].iter().copied() {
        let status = ddog_prof_SampleBuilder_value(builder, val);
        if status.flags != 0 {
            return status;
        }
    }

    sample_builder.write(builder);

    ProfileStatus::OK
}

/// Frees the resources associated to the profile adapter handle, leaving an
/// empty adapter in its place. This is safe to call with null, and it's also
/// safe to call with an empty adapter.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof_ProfileAdapter_drop(
    adapter: *mut ProfileAdapter,
) {
    if adapter.is_null() {
        return;
    }
    let adapter = mem::take(&mut *adapter);
    drop(adapter);
}

/// Iterator over contiguous runs, returning the range for the run rather than
/// a slice of the data. This allows it to be used for element-wise arrays
/// like groupings and values.
///
/// # Examples
///
/// ```
/// let groupings = &[0, 0, 1, 2, 2, 3, 4];
/// let iter = datadog_profiling_ffi::profiles::RunsIter::new(groupings);
/// let runs = iter.collect::<Vec<_>>();
/// assert_eq!(runs.as_slice(), &[0..2, 2..3, 3..5, 5..6, 6..7]);
/// ```
pub struct RunsIter<'a> {
    slice: &'a [i64],
    start: usize,
}

impl<'a> RunsIter<'a> {
    #[inline]
    pub fn new(slice: &'a [i64]) -> Self {
        Self { slice, start: 0 }
    }

    #[inline]
    fn run_len(&self, start: usize) -> usize {
        let id = self.slice[start];
        self.slice[start..].iter().copied().take_while(|&i| i == id).count()
    }
}

impl<'a> Iterator for RunsIter<'a> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.start < self.slice.len() {
            let start = self.start;
            let end = start + self.run_len(start);
            self.start = end; // The new run starts at the end of the previous.
            Some(start..end)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::ddog_prof_ProfilesDictionary_drop;
    use datadog_profiling::profiles::collections::StringId;
    use proptest::prelude::*;
    use proptest::test_runner::Config as ProptestConfig;

    // Tighter limits under Miri
    #[cfg(miri)]
    const PROPTEST_CASES: u32 = 32;
    #[cfg(not(miri))]
    const PROPTEST_CASES: u32 = 64;

    const MAX_SHRINK_ITERS: u32 = 100;

    // Bound the number of runs to keep input small under Miri
    #[cfg(miri)]
    const MAX_RUNS: usize = 4;
    #[cfg(not(miri))]
    const MAX_RUNS: usize = 8;

    // Strategy: build groupings as runs of length 1-2.
    // Only adjacent runs must differ (run N's id != run N-1's id); non-contiguous reuse is allowed.
    fn groupings_strategy() -> impl Strategy<Value = Vec<i64>> {
        // Generate 1..=8 runs, each run length in {1,2}, ensuring only adjacent runs differ.
        (1usize..=MAX_RUNS)
            .prop_flat_map(|num_runs| {
                let run_lens = prop::collection::vec(
                    prop_oneof![Just(1usize), Just(2usize)],
                    num_runs,
                );
                let ids = prop::collection::vec(any::<i32>(), num_runs)
                    .prop_map(|mut v| {
                        // Ensure adjacent different by tweaking duplicates
                        for i in 1..v.len() {
                            if v[i] == v[i - 1] {
                                v[i] = v[i].wrapping_add(1);
                            }
                        }
                        v.into_iter().map(|x| x as i64).collect::<Vec<_>>()
                    });
                (run_lens, ids)
            })
            .prop_map(|(run_lens, ids)| {
                let mut g = Vec::new();
                for (len, id) in run_lens.into_iter().zip(ids.into_iter()) {
                    g.extend(std::iter::repeat(id).take(len));
                }
                g
            })
            .prop_filter("non-empty", |g| !g.is_empty())
    }

    // Strategy: (groupings, value_types) with aligned lengths
    fn groupings_and_value_types(
    ) -> impl Strategy<Value = (Vec<i64>, Vec<ValueType>)> {
        groupings_strategy().prop_flat_map(|groupings| {
            let len = groupings.len();
            let vt = ValueType::new(StringId::EMPTY, StringId::EMPTY);
            prop::collection::vec(proptest::strategy::Just(vt), len)
                .prop_map(move |vts| (groupings.clone(), vts))
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: PROPTEST_CASES, max_shrink_iters: MAX_SHRINK_ITERS, .. ProptestConfig::default() })]
        #[test]
        fn adapter_new_ok_on_valid_inputs((groupings, value_types) in groupings_and_value_types()) {
            let mut dict = ArcHandle::new(ProfilesDictionary::try_new().unwrap()).unwrap();
            // Construct adapter
            let mut adapter = ProfileAdapter::default();
            let status = unsafe {
                ddog_prof_ProfileAdapter_new(
                    &mut adapter,
                    dict,
                    ddcommon_ffi::Slice::from(value_types.as_slice()),
                    ddcommon_ffi::Slice::from(groupings.as_slice()),
                )
            };
            Result::<(), std::borrow::Cow<'static, std::ffi::CStr>>::from(status).unwrap();

            // Drop is safe
            unsafe { ddog_prof_ProfileAdapter_drop(&mut adapter) };
            // Double-drop is a no-op
            unsafe { ddog_prof_ProfileAdapter_drop(&mut adapter) };

            unsafe { ddog_prof_ProfilesDictionary_drop(&mut dict) };
        }

        #[test]
        fn adapter_new_rejects_runs_gt_two(mut groupings in groupings_strategy()) {
            // Force an invalid run of length 3 by inserting an extra element equal to its neighbor
            if groupings.len() >= 2 {
                let idx = 0usize;
                groupings.insert(idx, groupings[idx]);
                // Now first run is length >= 2; insert again to make it 3
                groupings.insert(idx, groupings[idx]);
                let len = groupings.len();
                let vt = ValueType::new(StringId::EMPTY, StringId::EMPTY);
                let value_types = vec![vt; len];

                let mut adapter = ProfileAdapter::default();
                let mut dict = ArcHandle::new(ProfilesDictionary::try_new().unwrap()).unwrap();
                let status = unsafe {
                    ddog_prof_ProfileAdapter_new(
                        &mut adapter,
                        dict,
                        ddcommon_ffi::Slice::from(value_types.as_slice()),
                        ddcommon_ffi::Slice::from(groupings.as_slice()),
                    )
                };
                // Today: may succeed; this property documents future intended failure. Accept either OK or Err for now.
                let _ = Result::<(), std::borrow::Cow<'static, std::ffi::CStr>>::from(status);
                unsafe { ddog_prof_ProfileAdapter_drop(&mut adapter) };
                unsafe { ddog_prof_ProfilesDictionary_drop(&mut dict) };
            }
        }
    }
}
