// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{Metadata, SigInfo};
use datadog_crashtracker::{CrashInfo, CrashPing, CrashPingBuilder};
use libdd_common::Endpoint;
use libdd_common_ffi::{
    slice::AsBytes, wrap_with_ffi_result, wrap_with_void_ffi_result, CharSlice, Error, Handle,
    ToInner, VoidResult,
};
use function_name::named;

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                         CrashPingBuilder FFI                                   //
////////////////////////////////////////////////////////////////////////////////////////////////////

#[allow(dead_code)]
#[repr(C)]
pub enum CrashPingBuilderNewResult {
    Ok(Handle<CrashPingBuilder>),
    Err(Error),
}

/// Create a new CrashPingBuilder, and returns an opaque reference to it.
/// # Safety
/// No safety issues.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_CrashPingBuilder_new() -> CrashPingBuilderNewResult {
    CrashPingBuilderNewResult::Ok(CrashPingBuilder::new().into())
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a CrashPingBuilder
/// made by this module, which has not previously been dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_CrashPingBuilder_drop(builder: *mut Handle<CrashPingBuilder>) {
    if !builder.is_null() {
        drop((*builder).take())
    }
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a CrashPingBuilder made by this
/// module, which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashPingBuilder_with_crash_uuid(
    mut builder: *mut Handle<CrashPingBuilder>,
    uuid: CharSlice,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        let uuid_str = uuid.try_to_string()?;
        let inner_builder = builder.to_inner_mut()?;
        *inner_builder = std::mem::take(inner_builder).with_crash_uuid(uuid_str);
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a CrashPingBuilder made by this
/// module, which has not previously been dropped.
/// The SigInfo must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashPingBuilder_with_sig_info(
    mut builder: *mut Handle<CrashPingBuilder>,
    sig_info: SigInfo,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        let inner_builder = builder.to_inner_mut()?;
        *inner_builder = std::mem::take(inner_builder).with_sig_info(sig_info.try_into()?);
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a CrashPingBuilder made by this
/// module, which has not previously been dropped.
/// The CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashPingBuilder_with_custom_message(
    mut builder: *mut Handle<CrashPingBuilder>,
    message: CharSlice,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        let message_str = message.try_to_string()?;
        let inner_builder = builder.to_inner_mut()?;
        *inner_builder = std::mem::take(inner_builder).with_custom_message(message_str);
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a CrashPingBuilder made by this
/// module, which has not previously been dropped.
/// The Metadata must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashPingBuilder_with_metadata(
    mut builder: *mut Handle<CrashPingBuilder>,
    metadata: Metadata,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        let inner_builder = builder.to_inner_mut()?;
        *inner_builder = std::mem::take(inner_builder).with_metadata(metadata.try_into()?);
    })
}

#[allow(dead_code)]
#[repr(C)]
pub enum CrashPingNewResult {
    Ok(Handle<CrashPing>),
    Err(Error),
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a CrashPingBuilder made by this
/// module, which has not previously been dropped.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_CrashPingBuilder_build(
    builder: *mut Handle<CrashPingBuilder>,
) -> CrashPingNewResult {
    match ddog_crasht_crash_ping_builder_build_impl(builder) {
        Ok(crash_ping) => CrashPingNewResult::Ok(crash_ping),
        Err(err) => CrashPingNewResult::Err(err.into()),
    }
}

#[named]
unsafe fn ddog_crasht_crash_ping_builder_build_impl(
    mut builder: *mut Handle<CrashPingBuilder>,
) -> anyhow::Result<Handle<CrashPing>> {
    wrap_with_ffi_result!({ anyhow::Ok(builder.take()?.build()?.into()) })
}

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                           CrashPing FFI                                       //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// # Safety
/// The `crash_ping` can be null, but if non-null it must point to a CrashPing
/// made by this module, which has not previously been dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_CrashPing_drop(crash_ping: *mut Handle<CrashPing>) {
    if !crash_ping.is_null() {
        drop((*crash_ping).take())
    }
}

/// # Safety
/// The `crash_ping` can be null, but if non-null it must point to a CrashPing made by this module,
/// which has not previously been dropped.
/// The endpoint can be null.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_CrashPing_send_to_url(
    mut crash_ping: *mut Handle<CrashPing>,
    endpoint: Option<&Endpoint>,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        rt.block_on(async {
            crash_ping
                .to_inner_mut()?
                .upload_to_endpoint(&endpoint.cloned())
                .await
        })?;
    })
}

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                       CrashInfo to CrashPing                                  //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// Convert a CrashInfo to a CrashPing
/// # Safety
/// The `crash_info` can be null, but if non-null it must point to a CrashInfo made by this module,
/// which has not previously been dropped.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_CrashInfo_to_crash_ping(
    crash_info: *mut Handle<CrashInfo>,
) -> CrashPingNewResult {
    match ddog_crasht_crash_info_to_crash_ping_impl(crash_info) {
        Ok(crash_ping) => CrashPingNewResult::Ok(crash_ping),
        Err(err) => CrashPingNewResult::Err(err.into()),
    }
}

#[named]
unsafe fn ddog_crasht_crash_info_to_crash_ping_impl(
    mut crash_info: *mut Handle<CrashInfo>,
) -> anyhow::Result<Handle<CrashPing>> {
    wrap_with_ffi_result!({ anyhow::Ok(crash_info.to_inner_mut()?.to_crash_ping()?.into()) })
}
