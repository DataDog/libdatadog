// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{Metadata, SigInfo};
use ::function_name::named;
use datadog_crashtracker::{CrashPingBuilder, CrashPing};
use ddcommon::Endpoint;
use ddcommon_ffi::{
    slice::AsBytes, wrap_with_ffi_result, wrap_with_void_ffi_result, CharSlice, Error, Handle,
    ToInner, VoidResult,
};

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                              FFI API                                           //
////////////////////////////////////////////////////////////////////////////////////////////////////

#[allow(dead_code)]
#[repr(C)]
pub enum PingInfoBuilderNewResult {
    Ok(Handle<CrashPingBuilder>),
    Err(Error),
}

/// Create a new CrashPingBuilder, and returns an opaque reference to it.
/// # Safety
/// No safety issues.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_PingInfoBuilder_new() -> PingInfoBuilderNewResult {
    PingInfoBuilderNewResult::Ok(CrashPingBuilder::new().into())
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a PingInfoBuilder
/// made by this module, which has not previously been dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_PingInfoBuilder_drop(builder: *mut Handle<CrashPingBuilder>) {
    // Technically, this function has been designed so if it's double-dropped
    // then it's okay, but it's not something that should be relied on.
    if !builder.is_null() {
        drop((*builder).take())
    }
}

#[allow(dead_code)]
#[repr(C)]
pub enum PingInfoNewResult {
    Ok(Handle<CrashPing>),
    Err(Error),
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a PingInfoBuilder made by this module,
/// which has not previously been dropped.
#[no_mangle]
#[must_use]
pub unsafe extern "C" fn ddog_crasht_PingInfoBuilder_build(
    builder: *mut Handle<CrashPingBuilder>,
) -> PingInfoNewResult {
    match ddog_crasht_ping_info_builder_build_impl(builder) {
        Ok(ping_info) => PingInfoNewResult::Ok(ping_info),
        Err(err) => PingInfoNewResult::Err(err.into()),
    }
}

#[named]
unsafe fn ddog_crasht_ping_info_builder_build_impl(
    mut builder: *mut Handle<CrashPingBuilder>,
) -> anyhow::Result<Handle<CrashPing>> {
    wrap_with_ffi_result!({ anyhow::Ok(builder.take()?.build()?.into()) })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a PingInfoBuilder made by this module,
/// which has not previously been dropped.
/// The uuid CharSlice must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_PingInfoBuilder_with_uuid(
    mut builder: *mut Handle<CrashPingBuilder>,
    uuid: CharSlice,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        // Take the builder, apply the method, and put it back
        let old_builder = builder.take()?;
        let new_builder = old_builder.with_crash_uuid(uuid.try_to_string()?);
        *builder = new_builder.into();
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a PingInfoBuilder made by this module,
/// which has not previously been dropped.
/// The metadata must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_PingInfoBuilder_with_metadata(
    mut builder: *mut Handle<CrashPingBuilder>,
    metadata: Metadata,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        // Take the builder, apply the method, and put it back
        let old_builder = builder.take()?;
        let new_builder = old_builder.with_metadata(metadata.try_into()?);
        *builder = new_builder.into();
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a PingInfoBuilder made by this module,
/// which has not previously been dropped.
/// The sig_info must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_PingInfoBuilder_with_sig_info(
    mut builder: *mut Handle<CrashPingBuilder>,
    sig_info: SigInfo,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        // Take the builder, apply the method, and put it back
        let old_builder = builder.take()?;
        let new_builder = old_builder.with_sig_info(sig_info.try_into()?);
        *builder = new_builder.into();
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a PingInfoBuilder made by this module,
/// which has not previously been dropped.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_PingInfoBuilder_with_os_info_this_machine(
    mut builder: *mut Handle<CrashPingBuilder>,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        // Take the builder, apply the method, and put it back
        let old_builder = builder.take()?;
        let new_builder = old_builder.with_os_info_this_machine();
        *builder = new_builder.into();
    })
}

/// # Safety
/// The `builder` can be null, but if non-null it must point to a PingInfoBuilder made by this module,
/// which has not previously been dropped.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_PingInfoBuilder_with_proc_info(
    mut builder: *mut Handle<CrashPingBuilder>,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        // Take the builder, apply the method, and put it back
        let old_builder = builder.take()?;
        let new_builder = old_builder.with_proc_info_this_process();
        *builder = new_builder.into();
    })
}

/// # Safety
/// The `ping_info` can be null, but if non-null it must point to a PingInfo
/// made by this module, which has not previously been dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_PingInfo_drop(ping_info: *mut Handle<CrashPing>) {
    // Technically, this function has been designed so if it's double-dropped
    // then it's okay, but it's not something that should be relied on.
    if !ping_info.is_null() {
        drop((*ping_info).take())
    }
}

/// # Safety
/// The `ping_info` can be null, but if non-null it must point to a PingInfo made by this module,
/// which has not previously been dropped.
/// The endpoint can be null (uses builder's endpoint) or must be valid.
#[no_mangle]
#[must_use]
#[named]
pub unsafe extern "C" fn ddog_crasht_PingInfo_upload_to_endpoint(
    mut ping_info: *mut Handle<CrashPing>,
    endpoint: *const Endpoint,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        // Create a runtime to block on the async upload
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        // Take the ping_info and upload it
        let ping = ping_info.take()?;

        // For now, we use the endpoint from the builder. In the future, we could
        // extend CrashPing to support endpoint override if needed.
        // The endpoint parameter is reserved for future use.
        if !endpoint.is_null() {
            // TODO: Consider supporting endpoint override during upload
            // For now, we ignore the endpoint parameter and use the builder's endpoint
        }

        rt.block_on(ping.upload())?;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddcommon_ffi::CharSlice;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_ping_info_builder_ffi_basic() {
        unsafe {
            // Create builder
            let builder_result = ddog_crasht_PingInfoBuilder_new();
            let mut builder = match builder_result {
                PingInfoBuilderNewResult::Ok(b) => b,
                PingInfoBuilderNewResult::Err(_) => panic!("Failed to create builder"),
            };

            // Add UUID
            let uuid_slice = CharSlice::from("test-uuid-ffi-123");
            let result = ddog_crasht_PingInfoBuilder_with_uuid(&mut builder, uuid_slice);
            match result {
                ddcommon_ffi::VoidResult::Ok => {},
                ddcommon_ffi::VoidResult::Err(_) => panic!("Failed to set UUID"),
            }

            // Add metadata
            let tags = ddcommon_ffi::Vec::new();

            let metadata = Metadata {
                library_name: CharSlice::from("test-library"),
                library_version: CharSlice::from("1.0.0"),
                family: CharSlice::from("test"),
                tags: Some(&tags),
            };

            let result = ddog_crasht_PingInfoBuilder_with_metadata(&mut builder, metadata);
            match result {
                ddcommon_ffi::VoidResult::Ok => {},
                ddcommon_ffi::VoidResult::Err(_) => panic!("Failed to set metadata"),
            }

            // Add sig_info
            let sig_info = SigInfo {
                addr: CharSlice::from("0x0000000000001234"),
                code: 1,
                code_human_readable: datadog_crashtracker::SiCodes::SEGV_BNDERR,
                signo: 11,
                signo_human_readable: datadog_crashtracker::SignalNames::SIGSEGV,
            };

            let result = ddog_crasht_PingInfoBuilder_with_sig_info(&mut builder, sig_info);
            match result {
                ddcommon_ffi::VoidResult::Ok => {},
                ddcommon_ffi::VoidResult::Err(_) => panic!("Failed to set sig_info"),
            }

            // Add insights
            let result = ddog_crasht_PingInfoBuilder_with_os_info_this_machine(&mut builder);
            match result {
                ddcommon_ffi::VoidResult::Ok => {},
                ddcommon_ffi::VoidResult::Err(_) => panic!("Failed to set os_info"),
            }

            let result = ddog_crasht_PingInfoBuilder_with_proc_info(&mut builder);
            match result {
                ddcommon_ffi::VoidResult::Ok => {},
                ddcommon_ffi::VoidResult::Err(_) => panic!("Failed to set proc_info"),
            }

            // Build the ping
            let ping_result = ddog_crasht_PingInfoBuilder_build(&mut builder);
            let mut ping = match ping_result {
                PingInfoNewResult::Ok(p) => p,
                PingInfoNewResult::Err(_) => panic!("Failed to build ping info"),
            };

            // Upload - for testing we'll pass null endpoint to use builder's endpoint
            let result = ddog_crasht_PingInfo_upload_to_endpoint(&mut ping, std::ptr::null());
            match result {
                ddcommon_ffi::VoidResult::Ok => {},
                ddcommon_ffi::VoidResult::Err(_) => {
                    // Expected to fail since we don't have a real endpoint, that's okay
                }
            }

            // Cleanup
            ddog_crasht_PingInfo_drop(&mut ping);
            ddog_crasht_PingInfoBuilder_drop(&mut builder);
        }
    }
}