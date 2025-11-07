// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{Metadata, OsInfo, ProcInfo, SigInfo, Span, ThreadData};
use ::function_name::named;
use libdd_crashtracker::{CrashInfo, CrashInfoBuilder, ErrorKind, StackTrace};
use libdd_common_ffi::{
    slice::AsBytes, wrap_with_ffi_result, wrap_with_void_ffi_result, CharSlice, Error, Handle,
    Slice, Timespec, ToInner, VoidResult,
};

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                              FFI API                                           //
////////////////////////////////////////////////////////////////////////////////////////////////////

#[allow(dead_code)]
#[repr(C)]
pub enum CrashInfoBuilderNewResult {
    Ok(Handle<CrashInfoBuilder>),
    Err(Error),
}

/// Create a new CrashInfoBuilder, and returns an opaque reference to it.
/// # Safety
/// No safety issues.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_new() -> CrashInfoBuilderNewResult {
    CrashInfoBuilderNewResult::Ok(CrashInfoBuilder::new().into())
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

#[allow(dead_code)]
#[repr(C)]
pub enum CrashInfoNewResult {
    Ok(Handle<CrashInfo>),
    Err(Error),
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_build(
    builder: *mut Handle<CrashInfoBuilder>,
) -> CrashInfoNewResult {
    match ddog_crasht_crash_info_builder_build_impl(builder) {
        Ok(crash_info) => CrashInfoNewResult::Ok(crash_info),
        Err(err) => CrashInfoNewResult::Err(err.into()),
    }
}

#[named]
unsafe fn ddog_crasht_crash_info_builder_build_impl(
    mut builder: *mut Handle<CrashInfoBuilder>,
) -> anyhow::Result<Handle<CrashInfo>> {
    wrap_with_ffi_result!({ anyhow::Ok(builder.take()?.build()?.into()) })
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
    wrap_with_void_ffi_result!({
        builder
            .to_inner_mut()?
            .with_counter(name.try_to_string()?, value)?;
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
    wrap_with_void_ffi_result!({
        builder.to_inner_mut()?.with_kind(kind)?;
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
) -> VoidResult {
    wrap_with_void_ffi_result!({
        builder.to_inner_mut()?.with_file(
            filename
                .try_to_string_option()?
                .context("filename cannot be empty string")?,
        )?;
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_file_and_contents(
    mut builder: *mut Handle<CrashInfoBuilder>,
    filename: CharSlice,
    contents: Slice<CharSlice>,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        let filename = filename
            .try_to_string_option()?
            .context("filename cannot be empty string")?;
        let contents = {
            let mut accum = Vec::with_capacity(contents.len());
            for line in contents.iter() {
                let line = line.try_to_string()?;
                accum.push(line);
            }
            accum
        };

        builder
            .to_inner_mut()?
            .with_file_and_contents(filename, contents)?;
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
    wrap_with_void_ffi_result!({
        builder
            .to_inner_mut()?
            .with_fingerprint(fingerprint.try_to_string()?)?;
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
    wrap_with_void_ffi_result!({
        builder.to_inner_mut()?.with_incomplete(incomplete)?;
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
    also_print: bool,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        builder
            .to_inner_mut()?
            .with_log_message(message.try_to_string()?, also_print)?;
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
    wrap_with_void_ffi_result!({
        builder
            .to_inner_mut()?
            .with_metadata(metadata.try_into()?)?;
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
    wrap_with_void_ffi_result!({
        builder.to_inner_mut()?.with_os_info(os_info.try_into()?)?;
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
    wrap_with_void_ffi_result!({
        builder.to_inner_mut()?.with_os_info_this_machine()?;
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
    wrap_with_void_ffi_result!({
        builder
            .to_inner_mut()?
            .with_proc_info(proc_info.try_into()?)?;
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
    wrap_with_void_ffi_result!({
        builder
            .to_inner_mut()?
            .with_sig_info(sig_info.try_into()?)?;
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
    wrap_with_void_ffi_result!({
        builder.to_inner_mut()?.with_span_id(span_id.try_into()?)?;
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
    wrap_with_void_ffi_result!({
        builder.to_inner_mut()?.with_stack(*stack.take()?)?;
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
    mut thread: ThreadData,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        if thread.crashed {
            let stack = (thread.stack.to_inner_mut())?.clone();
            builder.to_inner_mut()?.with_stack(stack)?;
        }
        builder.to_inner_mut()?.with_thread(thread.try_into()?)?;
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
    wrap_with_void_ffi_result!({
        builder.to_inner_mut()?.with_timestamp(ts.into())?;
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
    wrap_with_void_ffi_result!({
        builder.to_inner_mut()?.with_timestamp_now()?;
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
    wrap_with_void_ffi_result!({
        builder
            .to_inner_mut()?
            .with_trace_id(trace_id.try_into()?)?;
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
    wrap_with_void_ffi_result!({
        let uuid = uuid
            .try_to_string_option()?
            .context("UUID cannot be empty string")?;
        builder.to_inner_mut()?.with_uuid(uuid)?;
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
    wrap_with_void_ffi_result!({
        builder.to_inner_mut()?.with_uuid_random()?;
    })
}

/// # Safety
/// The `crash_info` can be null, but if non-null it must point to a Builder made by this module,
/// which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashInfoBuilder_with_message(
    mut builder: *mut Handle<CrashInfoBuilder>,
    message: CharSlice,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        let message = message
            .try_to_string_option()?
            .context("message cannot be empty string")?;
        builder.to_inner_mut()?.with_message(message)?;
    })
}
