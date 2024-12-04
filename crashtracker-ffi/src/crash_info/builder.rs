// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{Metadata, OsInfo, ProcInfo, SigInfo, Span, ThreadData};
use ::function_name::named;
use anyhow::Context;
use datadog_crashtracker::rfc5_crash_info::{CrashInfo, CrashInfoBuilder, ErrorKind, StackTrace};
use ddcommon_ffi::{
    slice::AsBytes, wrap_with_ffi_result, CharSlice, Handle, Result, Slice, Timespec, ToInner,
    VoidResult,
};

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                              FFI API                                           //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// Create a new CrashInfoBuilder, and returns an opaque reference to it.
/// # Safety
/// No safety issues.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_new() -> Result<Handle<CrashInfoBuilder>> {
    ddcommon_ffi::Result::Ok(CrashInfoBuilder::new().into())
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Frame
/// made by this module, which has not previously been dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_drop(builder: *mut Handle<CrashInfoBuilder>) {
    // Technically, this function has been designed so if it's double-dropped
    // then it's okay, but it's not something that should be relied on.
    if !builder.is_null() {
        drop((*builder).take())
    }
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_build(
    mut builder: *mut Handle<CrashInfoBuilder>,
) -> Result<Handle<CrashInfo>> {
    wrap_with_ffi_result!({
        anyhow::ensure!(!builder.is_null());
        Ok(builder.take()?.build()?.into())
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_counter(
    mut builder: *mut Handle<CrashInfoBuilder>,
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
    mut builder: *mut Handle<CrashInfoBuilder>,
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
    mut builder: *mut Handle<CrashInfoBuilder>,
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
    mut builder: *mut Handle<CrashInfoBuilder>,
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
    mut builder: *mut Handle<CrashInfoBuilder>,
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
    mut builder: *mut Handle<CrashInfoBuilder>,
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
    mut builder: *mut Handle<CrashInfoBuilder>,
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
    mut builder: *mut Handle<CrashInfoBuilder>,
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
    mut builder: *mut Handle<CrashInfoBuilder>,
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
    mut builder: *mut Handle<CrashInfoBuilder>,
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
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_sig_info(
    mut builder: *mut Handle<CrashInfoBuilder>,
    sig_info: SigInfo,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder.to_inner_mut()?.with_sig_info(sig_info.try_into()?);
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
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_span_id(
    mut builder: *mut Handle<CrashInfoBuilder>,
    span_id: Span,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder.to_inner_mut()?.with_span_id(span_id.try_into()?)?;
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
    mut builder: *mut Handle<CrashInfoBuilder>,
    mut stack: *mut Handle<StackTrace>,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder.to_inner_mut()?.with_stack(*stack.take()?);
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
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_thread(
    mut builder: *mut Handle<CrashInfoBuilder>,
    thread: ThreadData,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder.to_inner_mut()?.with_thread(thread.try_into()?)?;
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
    mut builder: *mut Handle<CrashInfoBuilder>,
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
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_timestamp_now(
    mut builder: *mut Handle<CrashInfoBuilder>,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder.to_inner_mut()?.with_timestamp_now();
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
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_trace_id(
    mut builder: *mut Handle<CrashInfoBuilder>,
    trace_id: Span,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder
            .to_inner_mut()?
            .with_trace_id(trace_id.try_into()?)?;
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
    mut builder: *mut Handle<CrashInfoBuilder>,
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
    mut builder: *mut Handle<CrashInfoBuilder>,
) -> VoidResult {
    wrap_with_ffi_result!({
        builder.to_inner_mut()?.with_uuid_random();
        anyhow::Ok(())
    })
}
