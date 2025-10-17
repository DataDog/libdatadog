// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profile_handle::ProfileHandle2;
use crate::wrap_with_ffi_result;
use crate::{ensure_non_null_out_parameter, ProfileStatus2};
use datadog_profiling2::exporter::EncodedProfile;
use datadog_profiling2::profiles::datatypes::{Profile2, ValueType2};
use datadog_profiling2::profiles::ProfileError;
use ddcommon_ffi::slice::ByteSlice;
use ddcommon_ffi::{Handle, ToInner};
use function_name::named;

/// Allocates a new `Profile` and writes a handle to `handle`.
///
/// # Safety
///
/// - `handle` must be non-null and valid for writes of `ProfileHandle<_>`.
/// - The written handle must be dropped via the matching drop function; see
///   [`ddog_prof2_Profile_drop`] for more details.
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_Profile_new(
    handle: *mut ProfileHandle2<Profile2>,
) -> ProfileStatus2 {
    ensure_non_null_out_parameter!(handle);
    ProfileStatus2::from(|| -> Result<(), ProfileError> {
        let h = ProfileHandle2::try_new(Profile2::default())?;
        unsafe { handle.write(h) };
        Ok(())
    }())
}

/// Adds a sample type to a profile.
///
/// # Safety
///
/// - `handle` must refer to a live `Profile` and is treated as a unique mutable reference for the
///   duration of the call (no aliasing mutations).
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_Profile_add_sample_type(
    mut handle: ProfileHandle2<Profile2>,
    vt: ValueType2,
) -> ProfileStatus2 {
    ProfileStatus2::from(|| -> Result<(), ProfileError> {
        let prof = unsafe { handle.as_inner_mut()? };
        prof.try_add_sample_type(vt)
    }())
}

/// Sets the period and adds its `ValueType` to the profile.
///
/// # Safety
///
/// - `handle` must refer to a live `Profile` and is treated as a unique mutable reference for the
///   duration of the call (no aliasing mutations).
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_Profile_add_period(
    mut handle: ProfileHandle2<Profile2>,
    period: i64,
    vt: ValueType2,
) -> ProfileStatus2 {
    ProfileStatus2::from(|| -> Result<(), ProfileError> {
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
pub unsafe extern "C" fn ddog_prof2_Profile_drop(handle: *mut ProfileHandle2<Profile2>) {
    if let Some(h) = handle.as_mut() {
        drop(h.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::{
        ddog_prof2_ProfileAdapter_drop, ddog_prof2_ProfileAdapter_new,
        ddog_prof2_ProfilesDictionary_drop, ddog_prof2_ScratchPad_drop, ProfileAdapter2,
    };
    use crate::{ddog_prof2_Status_drop, ArcHandle2};
    use datadog_profiling2::profiles::collections::StringId2;
    use datadog_profiling2::profiles::datatypes::{ProfilesDictionary2, ScratchPad};
    use proptest::prelude::*;
    use proptest::test_runner::Config as ProptestConfig;
    use std::ffi::CStr;

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
                let run_lens =
                    prop::collection::vec(prop_oneof![Just(1usize), Just(2usize)], num_runs);
                let ids = prop::collection::vec(any::<i32>(), num_runs).prop_map(|mut v| {
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
                    g.extend(std::iter::repeat_n(id, len));
                }
                g
            })
            .prop_filter("non-empty", |g| !g.is_empty())
    }

    // Strategy: (groupings, value_types) with aligned lengths
    fn groupings_and_value_types() -> impl Strategy<Value = (Vec<i64>, Vec<ValueType2>)> {
        groupings_strategy().prop_flat_map(|groupings| {
            let len = groupings.len();
            let vt = ValueType2::new(StringId2::EMPTY, StringId2::EMPTY);
            prop::collection::vec(proptest::strategy::Just(vt), len)
                .prop_map(move |vts| (groupings.clone(), vts))
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: PROPTEST_CASES, max_shrink_iters: MAX_SHRINK_ITERS, .. ProptestConfig::default() })]
        #[test]
        fn adapter_new_ok_on_valid_inputs((groupings, value_types) in groupings_and_value_types()) {
            let mut dict = ArcHandle2::new(ProfilesDictionary2::try_new().unwrap()).unwrap();
            let mut scratchpad = ArcHandle2::new(ScratchPad::try_new().unwrap()).unwrap();
            // Construct adapter
            let mut adapter = ProfileAdapter2::default();
            let mut status = unsafe {
                ddog_prof2_ProfileAdapter_new(
                    &mut adapter,
                    dict,
                    scratchpad,
                    ddcommon_ffi::Slice::from(value_types.as_slice()),
                    ddcommon_ffi::Slice::from(groupings.as_slice()),
                )
            };

            if status.flags != 0 {
                let cstr = unsafe { CStr::from_ptr(status.err) };
                let str = cstr.to_str().unwrap();
                eprintln!("profile adapter failed: {str}");
            }

            // Safe to call on OK too.
            unsafe { ddog_prof2_Status_drop(&mut status)};

            // Drop is safe
            unsafe { ddog_prof2_ProfileAdapter_drop(&mut adapter) };
            // Double-drop is a no-op
            unsafe { ddog_prof2_ProfileAdapter_drop(&mut adapter) };

            unsafe { ddog_prof2_ScratchPad_drop(&mut scratchpad) };
            unsafe { ddog_prof2_ProfilesDictionary_drop(&mut dict) };
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
                let vt = ValueType2::new(StringId2::EMPTY, StringId2::EMPTY);
                let value_types = vec![vt; len];

                let mut adapter = ProfileAdapter2::default();
                let mut dict = ArcHandle2::new(ProfilesDictionary2::try_new().unwrap()).unwrap();
                let mut scratchpad = ArcHandle2::new(ScratchPad::try_new().unwrap()).unwrap();
                let mut status = unsafe {
                    ddog_prof2_ProfileAdapter_new(
                        &mut adapter,
                        dict,
                        scratchpad,
                        ddcommon_ffi::Slice::from(value_types.as_slice()),
                        ddcommon_ffi::Slice::from(groupings.as_slice()),
                    )
                };

                if status.flags != 0 {
                    let cstr = unsafe { CStr::from_ptr(status.err) };
                    let str = cstr.to_str().unwrap();
                    eprintln!("profile adapter failed: {str}");
                }
                // Safe to call on OK too.
                unsafe { ddog_prof2_Status_drop(&mut status)};

                unsafe { ddog_prof2_ProfileAdapter_drop(&mut adapter) };
                unsafe { ddog_prof2_ScratchPad_drop(&mut scratchpad) };
                unsafe { ddog_prof2_ProfilesDictionary_drop(&mut dict) };
            }
        }
    }
}
