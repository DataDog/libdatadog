// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{to_inner::ToInner, Metadata, OsInfo, ProcInfo, StackTrace};
use ::function_name::named;
use anyhow::Context;
use datadog_crashtracker::rfc5_crash_info::ErrorKind;
use ddcommon_ffi::{
    slice::AsBytes, wrap_with_ffi_result, CharSlice, Result, Slice, Timespec, VoidResult,
};

/// Represents a CrashInfoBuilder. Do not access its member for any reason, only use
/// the C API functions on this struct.
#[repr(C)]
pub struct CrashInfoBuilder {
    // This may be null, but if not it will point to a valid CrashInfoBuilder.
    inner: *mut datadog_crashtracker::rfc5_crash_info::CrashInfoBuilder,
}

impl ToInner for CrashInfoBuilder {
    type Inner = datadog_crashtracker::rfc5_crash_info::CrashInfoBuilder;

    unsafe fn to_inner_mut(&mut self) -> anyhow::Result<&mut Self::Inner> {
        self.inner
            .as_mut()
            .context("inner pointer was null, indicates use after free")
    }
}

impl CrashInfoBuilder {
    pub(super) fn new() -> Self {
        CrashInfoBuilder {
            inner: Box::into_raw(Box::new(
                datadog_crashtracker::rfc5_crash_info::CrashInfoBuilder::new(),
            )),
        }
    }

    pub(super) fn take(
        &mut self,
    ) -> Option<Box<datadog_crashtracker::rfc5_crash_info::CrashInfoBuilder>> {
        // Leaving a null will help with double-free issues that can
        // arise in C. Of course, it's best to never get there in the
        // first place!
        let raw = std::mem::replace(&mut self.inner, std::ptr::null_mut());

        if raw.is_null() {
            None
        } else {
            Some(unsafe { Box::from_raw(raw) })
        }
    }
}

impl Drop for CrashInfoBuilder {
    fn drop(&mut self) {
        drop(self.take())
    }
}

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                              FFI API                                           //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// Create a new CrashInfoBuilder, and returns an opaque reference to it.
/// # Safety
/// No safety issues.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_new() -> Result<CrashInfoBuilder> {
    Ok(CrashInfoBuilder::new()).into()
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Frame
/// made by this module, which has not previously been dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_drop(builder: *mut CrashInfoBuilder) {
    // Technically, this function has been designed so if it's double-dropped
    // then it's okay, but it's not something that should be relied on.
    if !builder.is_null() {
        drop((*builder).take())
    }
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_counter(
    mut builder: *mut CrashInfoBuilder,
    name: CharSlice,
    value: i64,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder
            .to_inner_mut()?
            .with_counter(name.try_to_utf8()?.to_string(), value)?;
        anyhow::Ok(())
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// The Kind must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_kind(
    mut builder: *mut CrashInfoBuilder,
    kind: ErrorKind,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder.to_inner_mut()?.with_kind(kind)?;
        anyhow::Ok(())
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_file(
    mut builder: *mut CrashInfoBuilder,
    filename: CharSlice,
    contents: Slice<CharSlice>,
) -> VoidResult {
    wrap_with_ffi_result!({
        let filename = filename
            .try_to_string_option()?
            .context("filename cannot be empty string")?;
        let contents = {
            let mut accum = Vec::with_capacity(contents.len());
            for line in contents.iter() {
                let line = line.try_to_utf8()?.to_string();
                accum.push(line);
            }
            accum
        };

        builder.to_inner_mut()?.with_file(filename, contents);
        anyhow::Ok(())
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_fingerprint(
    mut builder: *mut CrashInfoBuilder,
    fingerprint: CharSlice,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder
            .to_inner_mut()?
            .with_fingerprint(fingerprint.try_to_utf8()?.to_string())?;
        anyhow::Ok(())
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_incomplete(
    mut builder: *mut CrashInfoBuilder,
    incomplete: bool,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder.to_inner_mut()?.with_incomplete(incomplete);
        anyhow::Ok(())
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_log_message(
    mut builder: *mut CrashInfoBuilder,
    message: CharSlice,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder
            .to_inner_mut()?
            .with_log_message(message.try_to_utf8()?.to_string())?;
        anyhow::Ok(())
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// All arguments must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_metadata(
    mut builder: *mut CrashInfoBuilder,
    metadata: Metadata,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder.to_inner_mut()?.with_metadata(metadata.try_into()?);
        anyhow::Ok(())
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// All arguments must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_os_info(
    mut builder: *mut CrashInfoBuilder,
    os_info: OsInfo,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder.to_inner_mut()?.with_os_info(os_info.try_into()?);
        anyhow::Ok(())
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// All arguments must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_os_info_this_machine(
    mut builder: *mut CrashInfoBuilder,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder.to_inner_mut()?.with_os_info_this_machine();
        anyhow::Ok(())
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// All arguments must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_proc_info(
    mut builder: *mut CrashInfoBuilder,
    proc_info: ProcInfo,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder
            .to_inner_mut()?
            .with_proc_info(proc_info.try_into()?);
        anyhow::Ok(())
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// All arguments must be valid.
/// Consumes the stack argument.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_stack(
    mut builder: *mut CrashInfoBuilder,
    mut stack: StackTrace,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder
            .to_inner_mut()?
            .with_stack(*stack.take().context("Stack was empty. Use after free?")?);
        anyhow::Ok(())
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_timestamp_now(
    mut builder: *mut CrashInfoBuilder,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder.to_inner_mut()?.with_timestamp_now();
        anyhow::Ok(())
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_timestamp(
    mut builder: *mut CrashInfoBuilder,
    ts: Timespec,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder.to_inner_mut()?.with_timestamp(ts.into());
        anyhow::Ok(())
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_uuid(
    mut builder: *mut CrashInfoBuilder,
    uuid: CharSlice,
) -> VoidResult {
    wrap_with_ffi_result!({
        let builder = builder.to_inner_mut()?;
        let uuid = uuid
            .try_to_string_option()?
            .context("UUID cannot be empty string")?;
        builder.with_uuid(uuid);
        anyhow::Ok(())
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_uuid_random(
    mut builder: *mut CrashInfoBuilder,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder.to_inner_mut()?.with_uuid_random();
        anyhow::Ok(())
    })
}

// with_proc_info
// with_sig_info
// with_span_ids
// with_stack
// with_threads
// with_trace_ids
