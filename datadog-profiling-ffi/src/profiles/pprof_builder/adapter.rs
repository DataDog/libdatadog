// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::{ddog_prof2_PprofBuilder_add_profile, ddog_prof2_PprofBuilder_add_profile_with_poisson_upscaling, ddog_prof2_PprofBuilder_add_profile_with_proportional_upscaling, ddog_prof2_PprofBuilder_build_compressed, ddog_prof2_PprofBuilder_new, ddog_prof2_SampleBuilder_drop, ddog_prof2_SampleBuilder_new, ddog_prof2_SampleBuilder_value, ddog_prof2_ScratchPad_drop, PoissonUpscalingRule2, ProportionalUpscalingRule2, SampleBuilder2, Utf8Option};
use crate::{ensure_non_null_out_parameter, profiles, ArcHandle2, ProfileHandle2, ProfileStatus2};
use datadog_profiling2::exporter::EncodedProfile;
use datadog_profiling2::profiles::datatypes::{
    Profile2, ProfilesDictionary2, ScratchPad, ValueType2, MAX_SAMPLE_TYPES,
};
use ddcommon_ffi::{Handle, Slice, Timespec};
use std::mem;
use std::ops::Range;
use std::time::SystemTime;

/// An adapter from the offset-based pprof format to the separate profiles
/// format that sort of mirrors the otel format. If you use this type, you are
/// expected to make a new one each profiling interval e.g. 60 seconds.
///
/// Don't mutate this directly. Its definition is available for FFI layout
/// reasons only.
#[repr(C)]
pub struct ProfileAdapter2<'a> {
    started_at: Timespec,
    dictionary: ArcHandle2<ProfilesDictionary2>,
    scratchpad: ArcHandle2<ScratchPad>,
    mappings: ddcommon_ffi::vec::Vec<ProfileAdapterMapping>,
    // A vec of slice of proportional rules. Uses an empty slice if the
    // profile doesn't have a registered upscaling rule.
    proportional_upscaling_rules: ddcommon_ffi::vec::Vec<Slice<'a, ProportionalUpscalingRule2<'a>>>,
    // A vec of poisson rules. Exclusive with proportional rules. If the
    // profile doesn't have a poisson rule, then it uses a sampling distance
    // of 0, which isn't a legal value internally.
    poisson_upscaling_rules: ddcommon_ffi::Vec<PoissonUpscalingRule2>,
}

impl Default for ProfileAdapter2<'_> {
    fn default() -> Self {
        Self {
            started_at: Timespec::from(SystemTime::now()),
            dictionary: Default::default(),
            scratchpad: Default::default(),
            mappings: Default::default(),
            proportional_upscaling_rules: Default::default(),
            poisson_upscaling_rules: Default::default(),
        }
    }
}

pub struct ProfileAdapterMapping {
    profile: ProfileHandle2<Profile2>,
    /// This is the range in the sample types/values array in the legacy API
    /// that corresponds to this mapping.
    range: Range<usize>,
}

impl Drop for ProfileAdapter2<'_> {
    fn drop(&mut self) {
        let mut mappings = mem::take(&mut self.mappings).into_std();
        for mut mapping in mappings.drain(..) {
            drop(unsafe { mapping.profile.take() })
        }

        self.dictionary.drop_resource();
        self.scratchpad.drop_resource();
    }
}

#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_ProfileAdapter_clear_scratchpad_data(
    adapter: Option<&mut ProfileAdapter2<'_>>,
    scratchpad: ArcHandle2<ScratchPad>,
) -> ProfileStatus2 {
    let Some(adapter) = adapter else {
        return ProfileStatus2::from(c"invalid input: null adapter passed to ddog_prof2_ProfileAdapter_add_proportional_upscaling");
    };

    let scratchpad = scratchpad.try_clone().expect("todo");

    for x in adapter.proportional_upscaling_rules.iter_mut() {
        *x = Slice::empty();
    }

    for x in adapter.poisson_upscaling_rules.iter_mut() {
        x.sum_offset = 0;
        x.count_offset = 0;
        x.sampling_distance = 0;
    }

    for mapping in adapter.mappings.iter_mut() {
        let profile = mapping.profile.as_inner_mut().expect("TODO");
        profile.samples.clear();
    }

    ddog_prof2_ScratchPad_drop(&mut adapter.scratchpad);
    adapter.scratchpad = scratchpad;

    ProfileStatus2::OK
}

/// Creates an adapter that maps the legacy offset-based sample model
/// (one flat list of sample types/values) into multiple Profiles, each with
/// 1–2 sample types.
///
/// Inputs must satisfy:
/// - `value_types.len() == groupings.len()`
/// - `value_types.len() > 0 && groupings.len() > 0`
/// - `groupings` is a sequence of contiguous "runs". Each run defines one Profile and must have
///   length 1 or 2. These groupings all define the same runs:
///     - `[ 0,  0,  1,  0,  0]`
///     - `[ 0,  0,  1,  2,  2]`
///     - `[13, 13,  0,  5,  5]`
///
/// On success, a handle to the new `ProfileAdapter` is written to `out`. Drop
/// it with `ddog_prof2_ProfileAdapter_drop`.
///
/// Here is a partial C example using some PHP profiles.
///
/// ```c
/// ddog_prof2_ProfilesDictionaryHandle dictionary = // ... ;
///
/// // Assume these ValueType entries were populated using your string table
/// // (type/unit ids). Order corresponds to the legacy offsets:
/// //   [wall-time, wall-samples, cpu-time, alloc-bytes, alloc-count]
/// ddog_prof2_ValueType value_types[5] = {
///     wall_time, wall_samples, cpu_time, alloc_bytes, alloc_count
/// };
/// int64_t groupings[5] = { 0, 0, 1, 2, 2 };
///
/// ddog_prof2_ScratchPadHandle scratchpad = // ... ;
/// ddog_prof2_ProfileAdapter adapter;
/// ddog_prof2_ProfileStatus st = ddog_prof2_ProfileAdapter_new(
///     &adapter,
///     dictionary,
///     scratchpad,
///     (ddog_Slice_ValueType){ .ptr = value_types, .len = 5 },
///     (ddog_Slice_I64){ .ptr = groupings, .len = 5 }
/// );
/// if (st.flags != 0) {
///     // handle error, then:
///     ddog_prof2_Status_drop(&st)
/// }
///
/// // ...later...
///
/// // Allocation sample was taken.
/// int64_t values[5] = { 0, 0, 0, 128, 1 };
/// ddog_Slice_I64 ffi_slice = { .ptr = values, len = 5 };
///
/// ddog_prof2_SampleBuilderHandle sample_builder_handle;
///
/// st = ddog_prof2_ProfileAdapter_add_sample(
///     &sample_builder_handle,
///     adapter,
///     2, // profile grouping 2
///     ffi_slice,
///     scratchpad,
/// );
///
/// // check st, then you can use SampleBuilder methods
/// // to add timestamps, links, etc.
///
/// // then add it to the profile:
/// st = ddog_prof2_SampleBuilder_finish(
///     &sample_builder_handle,
/// );
///
/// // add upscalings per profile grouping with one of:
/// // ddog_prof2_ProfileAdapter_add_poisson_upscaling
/// // ddog_prof2_ProfileAdapter_add_proportional_upscaling
///
///
/// // When the interval is up e.g. 60 seconds, then:
/// ddog_prof2_EndcodedProfile encoded_profile;
/// status = ddog_prof2_ProfileAdapter_build_compressed(
///     &encoded_profile,
///     &adapter, // this clears the adapter
///     NULL, // start time, if you want to provide one manually
///     NULL, // stop time, if you want to provide one manually
/// );
///
///
/// // order of these doesn't matter, the adapter keeps a refcount
/// // alive on the dictionary and scratchpad.
/// ddog_prof2_ProfilesDictionary_drop(&dictionary);
/// ddog_prof2_ScratchPad_drop(&scratchpad);
///
/// ddog_prof2_ProfileAdapter_drop(&adapter);
/// ```
///
/// # Safety
/// todo
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_ProfileAdapter_new(
    out: *mut ProfileAdapter2<'_>,
    dictionary: ArcHandle2<ProfilesDictionary2>,
    scratchpad: ArcHandle2<ScratchPad>,
    value_types: Slice<'_, ValueType2>,
    groupings: Slice<'_, i64>,
) -> ProfileStatus2 {
    // Ensure slices and inputs are valid.
    if out.is_null() {
        return ProfileStatus2::from(
            c"invalid input: argument out to ddog_prof2_ProfileAdapter_new was null",
        );
    }
    let Ok(value_types) = value_types.try_as_slice() else {
        return ProfileStatus2::from(c"invalid input: argument value_types to ddog_prof2_ProfileAdapter_new failed to convert to a Rust slice");
    };
    let Ok(groupings) = groupings.try_as_slice() else {
        return ProfileStatus2::from(c"invalid input: argument groupings to ddog_prof2_ProfileAdapter_new failed to convert to a Rust slice");
    };

    // Ensure the value_types and groupings have the same length.
    if value_types.len() != groupings.len() {
        return ProfileStatus2::from(c"invalid input: arguments value_types and groupings to ddog_prof2_ProfileAdapter_new had mismatched lengths");
    }
    // Ensure the slices have at least 1 element.
    if value_types.is_empty() {
        return ProfileStatus2::from(c"invalid input: arguments value_types and groupings to ddog_prof2_ProfileAdapter_new must not be empty");
    }

    // Count runs and validate max run length.
    let (n_runs, longest_run) = count_runs_and_longest_run(groupings);
    if longest_run > MAX_SAMPLE_TYPES {
        return ProfileStatus2::from(
            c"invalid input: groupings must appear in runs of length at most 2",
        );
    }

    // Build mapping of profiles (one per contiguous run).
    let mut mappings = ddcommon_ffi::vec::Vec::new();
    let mut proportional_upscaling_rules = ddcommon_ffi::vec::Vec::new();
    let mut poisson_upscaling_rules = ddcommon_ffi::vec::Vec::new();
    if mappings.try_reserve_exact(n_runs).is_err() {
        return ProfileStatus2::from(
            c"out of memory: couldn't reserve memory for ProfileAdapter's mappings",
        );
    }
    if proportional_upscaling_rules
        .try_reserve_exact(n_runs)
        .is_err()
    {
        return ProfileStatus2::from(c"out of memory: couldn't reserve memory for ProfileAdapter's proportional upscaling rules");
    }
    if poisson_upscaling_rules.try_reserve_exact(n_runs).is_err() {
        return ProfileStatus2::from(
            c"out of memory: couldn't reserve memory for ProfileAdapter's poisson upscaling rules",
        );
    }

    for run in RunsIter::new(groupings) {
        // Create a profile for this run
        let mut mapping = ProfileAdapterMapping {
            profile: Default::default(),
            range: Default::default(),
        };
        let result = profiles::ddog_prof2_Profile_new(&mut mapping.profile);
        if result.flags != 0 {
            return result;
        }
        mapping.range = run.clone();
        let profile = mapping.profile;
        mappings.push(mapping);
        proportional_upscaling_rules.push(Slice::default());
        poisson_upscaling_rules.push(PoissonUpscalingRule2 {
            sum_offset: 0,
            count_offset: 0,
            sampling_distance: 0,
        });
        // Add sample types for the run. The run length was previously
        // validated to be <= MAX_SAMPLE_TYPES.
        for value_idx in run {
            let status =
                profiles::ddog_prof2_Profile_add_sample_type(profile, value_types[value_idx]);
            if status.flags != 0 {
                return status;
            }
        }
    }

    let Ok(mut dictionary) = dictionary.try_clone() else {
        return ProfileStatus2::from(
            c"reference count overflow: profile adapter could not clone the profiles dictionary",
        );
    };

    let Ok(scratchpad) = scratchpad.try_clone() else {
        dictionary.drop_resource();
        return ProfileStatus2::from(
            c"reference count overflow: profile adapter could not clone the scratchpad",
        );
    };

    unsafe {
        out.write(ProfileAdapter2 {
            started_at: Timespec::from(SystemTime::now()),
            dictionary,
            scratchpad,
            mappings,
            proportional_upscaling_rules,
            poisson_upscaling_rules,
        })
    };
    ProfileStatus2::OK
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
///
/// # Safety
/// todo
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_ProfileAdapter_add_sample(
    sample_builder: *mut ProfileHandle2<SampleBuilder2>,
    adapter: &ProfileAdapter2<'_>,
    profile_grouping: usize,
    values: Slice<'_, i64>,
) -> ProfileStatus2 {
    assert!(!sample_builder.is_null());
    assert!(profile_grouping < adapter.mappings.len());
    if adapter.mappings.is_empty() {
        return ProfileStatus2::from(
            c"invalid input: ddog_prof2_ProfileAdapter_add_sample was called on an empty adapter",
        );
    }
    let Ok(values) = values.try_as_slice() else {
        return ProfileStatus2::from(c"invalid input: FFI values slice passed to ddog_prof2_ProfileAdapter_add_sample couldn't be converted to a Rust slice");
    };

    let Some(mapping) = adapter.mappings.get(profile_grouping) else {
        return ProfileStatus2::from(c"invalid input: grouping passed to ddog_prof2_ProfileAdapter_add_sample was out of range");
    };

    let mut builder = ProfileHandle2::default();
    let status = ddog_prof2_SampleBuilder_new(&mut builder, mapping.profile, adapter.scratchpad);
    if status.flags != 0 {
        return status;
    }
    for val in values[mapping.range.clone()].iter().copied() {
        let status = ddog_prof2_SampleBuilder_value(builder, val);
        if status.flags != 0 {
            ddog_prof2_SampleBuilder_drop(&mut builder);
            return status;
        }
    }

    sample_builder.write(builder);

    ProfileStatus2::OK
}

/// # Safety
/// todo
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_ProfileAdapter_add_proportional_upscaling<'a>(
    adapter: Option<&mut ProfileAdapter2<'a>>,
    grouping_index: usize,
    upscaling_rules: Slice<'a, ProportionalUpscalingRule2<'a>>,
    // utf8_option: Utf8Option, // todo: store this too
) -> ProfileStatus2 {
    let Some(adapter) = adapter else {
        return ProfileStatus2::from(c"invalid input: null adapter passed to ddog_prof2_ProfileAdapter_add_proportional_upscaling");
    };
    let Some(rules) = adapter.proportional_upscaling_rules.get_mut(grouping_index) else {
        return ProfileStatus2::from(c"invalid input: grouping index passed to ddog_prof2_ProfileAdapter_add_proportional_upscaling was out of range");
    };
    if !rules.is_empty() {
        return ProfileStatus2::from(c"invalid input: ddog_prof2_ProfileAdapter_add_proportional_upscaling was called for the same grouping more than once");
    }
    if let Some(rule) = adapter.poisson_upscaling_rules.get(grouping_index) {
        if rule.sampling_distance != 0 {
            return ProfileStatus2::from(c"invalid input: ddog_prof2_ProfileAdapter_add_proportional_upscaling was called on a grouping that already had a poisson rule");
        }
    }
    *rules = upscaling_rules;

    ProfileStatus2::OK
}

/// # Safety
/// todo
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_ProfileAdapter_add_poisson_upscaling(
    adapter: Option<&mut ProfileAdapter2<'_>>,
    grouping_index: usize,
    upscaling_rule: PoissonUpscalingRule2,
) -> ProfileStatus2 {
    let Some(adapter) = adapter else {
        return ProfileStatus2::from(
            c"invalid input: null adapter passed to ddog_prof2_ProfileAdapter_add_poisson_upscaling",
        );
    };

    if upscaling_rule.sampling_distance == 0 {
        return ProfileStatus2::from(c"invalid input: ddog_prof2_ProfileAdapter_add_poisson_upscaling cannot have a sampling distance of zero");
    }

    let Some(rule) = adapter.poisson_upscaling_rules.get_mut(grouping_index) else {
        return ProfileStatus2::from(c"invalid input: grouping index passed to ddog_prof2_ProfileAdapter_add_poisson_upscaling was out of range");
    };

    if rule.sampling_distance != 0 {
        return ProfileStatus2::from(c"invalid input: ddog_prof2_ProfileAdapter_add_poisson_upscaling was called for the same grouping more than once");
    }
    if let Some(rules) = adapter.proportional_upscaling_rules.get(grouping_index) {
        if !rules.is_empty() {
            return ProfileStatus2::from(c"invalid input: ddog_prof2_ProfileAdapter_add_poisson_upscaling was called on a grouping that already had proportional rules");
        }
    }

    *rule = upscaling_rule;

    ProfileStatus2::OK
}

/// Builds and compresses a pprof using the data in the profile adapter.
///
/// Afterward, you probably want to drop the adapter and make a new one.
///
/// # Parameters
///  * `out_profile`: a pointer safe for `core::ptr::write`ing the handle for the encoded profile.
///  * `adapter`: a mutable reference to the profile adapter.
///  * `start`: an optional reference to the start time of the Pprof profile. Defaults to the time
///    the adapter was made.
/// * `end`: an optional reference to the stop time of the Pprof profile. Defaults to the time this
///   call was made.
///
/// # Safety
/// todo
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_ProfileAdapter_build_compressed(
    out_profile: *mut Handle<EncodedProfile>,
    adapter: Option<&mut ProfileAdapter2<'_>>,
    start: Option<&Timespec>,
    end: Option<&Timespec>,
) -> ProfileStatus2 {
    ensure_non_null_out_parameter!(out_profile);
    let Some(adapter) = adapter else {
        return ProfileStatus2::from(
            c"invalid input: null adapter passed to ddog_prof2_ProfileAdapter_build_compressed",
        );
    };
    let start = *start.unwrap_or(&adapter.started_at);
    let end = end
        .cloned()
        .unwrap_or_else(|| Timespec::from(SystemTime::now()));

    let mut pprof_builder = ProfileHandle2::default();
    let Ok(dictionary) = adapter.dictionary.try_clone() else {
        return ProfileStatus2::from(c"reference count overflow: failed to increase refcount of profiles dictionary for ddog_prof2_ProfileAdapter_build_compressed");
    };
    let Ok(scratchpad) = adapter.scratchpad.try_clone() else {
        return ProfileStatus2::from(c"reference count overflow: failed to increase refcount of scratchpad for ddog_prof2_ProfileAdapter_build_compressed");
    };
    let status = ddog_prof2_PprofBuilder_new(&mut pprof_builder, dictionary, scratchpad);
    if status.flags != 0 {
        return status;
    }

    for grouping_index in 0..adapter.mappings.len() {
        let mapping = &adapter.mappings[grouping_index];
        let proportional = adapter.proportional_upscaling_rules[grouping_index];
        if !proportional.is_empty() {
            let status = ddog_prof2_PprofBuilder_add_profile_with_proportional_upscaling(
                pprof_builder,
                mapping.profile,
                proportional,
                Utf8Option::Assume,
            );
            if status.flags != 0 {
                return status;
            }
        } else {
            let poisson = adapter.poisson_upscaling_rules[grouping_index];

            let status = if poisson.sampling_distance != 0 {
                ddog_prof2_PprofBuilder_add_profile_with_poisson_upscaling(
                    pprof_builder,
                    mapping.profile,
                    poisson,
                )
            } else {
                ddog_prof2_PprofBuilder_add_profile(pprof_builder, mapping.profile)
            };
            if status.flags != 0 {
                return status;
            }
        }
    }

    // This is a limit of protobuf itself, the function will limit to a
    // smaller value around the current intake limits.
    let max_capacity = i32::MAX as u32;
    ddog_prof2_PprofBuilder_build_compressed(out_profile, pprof_builder, max_capacity, start, end)
}

/// Frees the resources associated to the profile adapter handle, leaving an
/// empty adapter in its place. This is safe to call with null, and it's also
/// safe to call with an empty adapter.
///
/// # Safety
/// todo
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_ProfileAdapter_drop(adapter: *mut ProfileAdapter2) {
    if adapter.is_null() {
        return;
    }
    drop(mem::take(&mut *adapter));
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
        self.slice[start..]
            .iter()
            .copied()
            .take_while(|&i| i == id)
            .count()
    }
}

impl Iterator for RunsIter<'_> {
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
