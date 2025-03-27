// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::slice::CharSlice;
#[cfg(target_os = "linux")]
use ddcommon::tracer_metadata::AnonymousFileHandle;
use ddcommon::tracer_metadata::{self, TracerMetadata};
use std::os::raw::c_int;

/// C-compatible representation of an anonymous file handle
#[repr(C)]
pub struct TracerMemfdHandle {
    /// File descriptor (relevant only on Linux)
    pub fd: c_int,
    /// Whether the handle is valid
    pub is_valid: bool,
}

/// Store tracer metadata to a file handle
///
/// # Safety
///
/// Accepts raw C-compatible strings
#[no_mangle]
pub unsafe extern "C" fn ddog_store_tracer_metadata(
    schema_version: u8,
    runtime_id: CharSlice,
    tracer_language: CharSlice,
    tracer_version: CharSlice,
    hostname: CharSlice,
    service_name: CharSlice,
    service_env: CharSlice,
    service_version: CharSlice,
) -> TracerMemfdHandle {
    // Convert C strings to Rust types
    let metadata = TracerMetadata {
        schema_version,
        runtime_id: if runtime_id.is_empty() {
            None
        } else {
            Some(runtime_id.to_string())
        },
        tracer_language: tracer_language.to_string(),
        tracer_version: tracer_version.to_string(),
        hostname: hostname.to_string(),
        service_name: if service_name.is_empty() {
            None
        } else {
            Some(service_name.to_string())
        },
        service_env: if service_env.is_empty() {
            None
        } else {
            Some(service_env.to_string())
        },
        service_version: if service_version.is_empty() {
            None
        } else {
            Some(service_version.to_string())
        },
    };

    // Call the actual implementation
    match tracer_metadata::store_tracer_metadata(&metadata) {
        #[cfg(target_os = "linux")]
        Ok(handle) => {
            use std::os::fd::{IntoRawFd, OwnedFd};
            let AnonymousFileHandle::Linux(memfd) = handle;
            let owned_fd: OwnedFd = memfd.into_file().into();
            TracerMemfdHandle {
                fd: owned_fd.into_raw_fd(),
                is_valid: true,
            }
        }
        #[cfg(not(target_os = "linux"))]
        Ok(_) => TracerMemfdHandle {
            fd: -1,
            is_valid: false,
        },
        Err(_err) => TracerMemfdHandle {
            fd: -1,
            is_valid: false,
        },
    }
}
