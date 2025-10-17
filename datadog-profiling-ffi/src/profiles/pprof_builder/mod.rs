// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod adapter;
mod upscaling;

pub use adapter::*;
pub use upscaling::*;

use crate::profile_handle::ProfileHandle2;
use crate::profiles::{ensure_non_null_out_parameter, Utf8ConversionError, Utf8Option};
use crate::{ArcHandle2, ProfileStatus2};
use datadog_profiling2::exporter::EncodedProfile;
use datadog_profiling2::profiles::datatypes::{Profile2, ProfilesDictionary2, ScratchPad};
use datadog_profiling2::profiles::{Compressor, PprofBuilder2, ProfileError, SizeRestrictedBuffer};
use ddcommon_ffi::slice::Slice;
use ddcommon_ffi::{Handle, Timespec};

/// Creates a `PprofBuilder` handle.
///
/// # Safety
///
/// - `out` must be non-null and valid for writes of `ProfileHandle<_>`.
/// - `dictionary` and `scratchpad` must be live handles whose resources outlive all uses of the
///   returned builder handle.
/// - Callers must uphold aliasing rules across FFI: while the builder is mutated through this
///   handle, no other references to the same builder may be used.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_PprofBuilder_new(
    out: *mut ProfileHandle2<PprofBuilder2<'_>>,
    dictionary: ArcHandle2<ProfilesDictionary2>,
    scratchpad: ArcHandle2<ScratchPad>,
) -> ProfileStatus2 {
    ensure_non_null_out_parameter!(out);
    ProfileStatus2::from(|| -> Result<(), ProfileError> {
        let dict = dictionary.as_inner()? as *const ProfilesDictionary2;
        let pad = scratchpad.as_inner()? as *const ScratchPad;
        // SAFETY: Tie lifetime to 'static for FFI; caller must ensure handles outlive builder
        // usage.
        let builder = PprofBuilder2::new(unsafe { &*dict }, unsafe { &*pad });
        let h = ProfileHandle2::try_new(builder)?;
        unsafe { out.write(h) };
        Ok(())
    }())
}

/// Adds a profile to the builder without upscaling rules.
///
/// # Safety
///
/// - `handle` must refer to a live builder, and no other mutable references to that builder may be
///   active for the duration of the call.
/// - `profile` must be non-null and point to a valid `Profile` that remains alive until the pprof
///   builder is done.
///
/// TODO: finish safety
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_PprofBuilder_add_profile(
    mut handle: ProfileHandle2<PprofBuilder2<'_>>,
    profile: ProfileHandle2<Profile2>,
) -> ProfileStatus2 {
    let profile = match profile.as_inner() {
        Ok(profile) => unsafe { core::mem::transmute::<&Profile2, &Profile2>(profile) },
        Err(err) => return ProfileStatus2::from_ffi_safe_error_message(err),
    };
    let result = || -> Result<(), ProfileStatus2> {
        let builder = unsafe {
            handle
                .as_inner_mut()
                .map_err(ProfileStatus2::from_ffi_safe_error_message)?
        };
        builder
            .try_add_profile(profile)
            .map_err(ProfileStatus2::from_ffi_safe_error_message)
    }();
    match result {
        Ok(_) => ProfileStatus2::OK,
        Err(err) => err,
    }
}

/// Adds a profile to the builder with the attached poisson upscaling rule.
///
/// # Safety
///
/// - `handle` must refer to a live builder, and no other mutable references to that builder may be
///   active for the duration of the call.
/// - `profile` must be non-null and point to a valid `Profile` that remains alive until the pprof
///   builder is done.
///
/// TODO: finish safety
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_PprofBuilder_add_profile_with_poisson_upscaling(
    mut handle: ProfileHandle2<PprofBuilder2<'_>>,
    profile: ProfileHandle2<Profile2>,
    upscaling_rule: PoissonUpscalingRule2,
) -> ProfileStatus2 {
    let profile = match profile.as_inner() {
        Ok(profile) => profile,
        Err(err) => return ProfileStatus2::from_ffi_safe_error_message(err),
    };
    let result = || -> Result<(), ProfileStatus2> {
        let builder = unsafe {
            handle
                .as_inner_mut()
                .map_err(ProfileStatus2::from_ffi_safe_error_message)?
        };

        let upscaling_rule = upscaling_rule
            .try_into()
            .map_err(ProfileStatus2::from_ffi_safe_error_message)?;
        builder
            .try_add_profile_with_poisson_upscaling(
                // SAFETY: todo lifetime extension
                unsafe { core::mem::transmute::<&Profile2, &Profile2>(profile) },
                upscaling_rule,
            )
            .map_err(ProfileStatus2::from_ffi_safe_error_message)
    }();
    match result {
        Ok(_) => ProfileStatus2::OK,
        Err(status) => status,
    }
}

/// Adds a profile to the builder with the attached proportional rule.
///
/// # Safety
///
/// - `handle` must refer to a live builder, and no other mutable references to that builder may be
///   active for the duration of the call.
/// - `profile` must be non-null and point to a valid `Profile` that remains alive until the pprof
///   builder is done.
///
/// TODO: finish safety
#[must_use]
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_PprofBuilder_add_profile_with_proportional_upscaling<'a>(
    mut handle: ProfileHandle2<PprofBuilder2<'a>>,
    profile: ProfileHandle2<Profile2>,
    upscaling_rules: Slice<ProportionalUpscalingRule2<'a>>,
    utf8_option: Utf8Option,
) -> ProfileStatus2 {
    let profile = match profile.as_inner() {
        Ok(profile) => profile,
        Err(err) => return ProfileStatus2::from_error(err),
    };
    let result = || -> Result<(), ProfileStatus2> {
        let builder = unsafe { handle.as_inner_mut() }
            .map_err(ProfileStatus2::from_ffi_safe_error_message)?;

        let upscaling_rules = upscaling_rules
            .try_as_slice()
            .map_err(ProfileStatus2::from_ffi_safe_error_message)?;

        builder
            .try_add_profile_with_proportional_upscaling(
                // SAFETY: todo lifetime extension
                unsafe { core::mem::transmute::<&Profile2, &Profile2>(profile) },
                upscaling_rules
                    .iter()
                    .map(|rule| -> Result<_, Utf8ConversionError> {
                        let key = rule.group_by_label.key;
                        let value = utf8_option.try_as_bytes_convert(rule.group_by_label.value)?;
                        Ok(((key, value), rule.sampled as f64 / rule.real as f64))
                    }),
            )
            .map_err(ProfileStatus2::from_ffi_safe_error_message)
    }();
    match result {
        Ok(_) => ProfileStatus2::OK,
        Err(status) => status,
    }
}

/// Builds and returns a compressed `EncodedProfile` via `out_profile`.
///
/// # Safety
///
/// - `out_profile` must be non-null and valid for writes of `Handle<_>`.
/// - `handle` must refer to a live builder whose dependencies (dictionary, scratchpad) are still
///   alive.
/// - No other references may concurrently mutate the same builder.
/// - `start` and `end` must denote a non-decreasing time range.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_PprofBuilder_build_compressed(
    out_profile: *mut Handle<EncodedProfile>,
    handle: ProfileHandle2<PprofBuilder2<'_>>,
    max_capacity: u32,
    start: Timespec,
    end: Timespec,
) -> ProfileStatus2 {
    build_with_sink::<Compressor, _, _>(
        out_profile,
        handle,
        max_capacity,
        start,
        end,
        |cap| Ok(Compressor::with_max_capacity(cap)),
        |mut c| c.finish(),
    )
}

/// Builds and returns an uncompressed `EncodedProfile` via `out_profile`.
///
/// # Safety
///
/// Same requirements as [`ddog_prof2_PprofBuilder_build_compressed`].
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_PprofBuilder_build_uncompressed(
    out_profile: *mut Handle<EncodedProfile>,
    handle: ProfileHandle2<PprofBuilder2<'static>>,
    size_hint: u32,
    start: Timespec,
    end: Timespec,
) -> ProfileStatus2 {
    build_with_sink::<SizeRestrictedBuffer, _, _>(
        out_profile,
        handle,
        size_hint,
        start,
        end,
        |cap| Ok(SizeRestrictedBuffer::new(cap)),
        |b| Ok(b.into()),
    )
}

fn build_with_sink<Sink, Make, Finalize>(
    out_profile: *mut Handle<EncodedProfile>,
    mut handle: ProfileHandle2<PprofBuilder2<'_>>,
    max_capacity: u32,
    start: Timespec,
    end: Timespec,
    make_sink: Make,
    finalize: Finalize,
) -> ProfileStatus2
where
    Sink: std::io::Write,
    Make: FnOnce(usize) -> Result<Sink, ProfileError>,
    Finalize: FnOnce(Sink) -> Result<Vec<u8>, ProfileError>,
{
    ensure_non_null_out_parameter!(out_profile);
    ProfileStatus2::from(|| -> Result<(), ProfileError> {
        if start.seconds > end.seconds
            || (start.seconds == end.seconds && start.nanoseconds > end.nanoseconds)
        {
            return Err(ProfileError::other("end time cannot be before start time"));
        }
        let builder = unsafe { handle.as_inner_mut()? };
        const MIB: usize = 1024 * 1024;
        // This is decoupled from the intake limit somewhat so that if the
        // limit is raised a little, clients don't need to be rebuilt. Of
        // course, if the limit is raised a lot then we'll need to rebuild
        // with a new max.
        let max_cap = (max_capacity as usize).min(64 * MIB);
        let mut sink = make_sink(max_cap)?;
        builder.build(&mut sink)?;
        let buffer = finalize(sink)?;
        let start: std::time::SystemTime = start.into();
        let end: std::time::SystemTime = end.into();
        let encoded = EncodedProfile {
            start,
            end,
            buffer,
            endpoints_stats: Default::default(),
        };
        let h = Handle::try_new(encoded).ok_or(ProfileError::other(
            "out of memory: failed to allocate handle for the EncodedProfile",
        ))?;
        unsafe { out_profile.write(h) };
        Ok(())
    }())
}

/// Drops the builder resource held by `handle` and leaves an empty handle.
///
/// # Safety
///
/// - If non-null, `handle` must point to a valid `ProfileHandle<PprofBuilder<'static>>`.
/// - The underlying resource must be dropped at most once across all copies of the handle. Calling
///   this on the same handle multiple times is ok.
/// - Do not use other copies of the handle after the resource is dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_prof2_PprofBuilder_drop(
    handle: *mut ProfileHandle2<PprofBuilder2<'static>>,
) {
    if let Some(h) = handle.as_mut() {
        drop(h.take());
    }
}
