// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(target_os = "linux")]
use datadog_library_config::tracer_metadata::AnonymousFileHandle;
use datadog_library_config::tracer_metadata::{self, TracerMetadata};
use ddcommon_ffi::Result;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int};

/// C-compatible representation of an anonymous file handle
#[repr(C)]
pub struct TracerMemfdHandle {
    /// File descriptor (relevant only on Linux)
    pub fd: c_int,
}

/// Represents the types of metadata that can be set on a `TracerMetadata` object.
#[repr(C)]
pub enum MetadataKind {
    RuntimeId = 0,
    TracerLanguage = 1,
    TracerVersion = 2,
    Hostname = 3,
    ServiceName = 4,
    ServiceEnv = 5,
    ServiceVersion = 6,
    ProcessTags = 7,
    ContainerId = 8,
}

/// Allocates and returns a pointer to a new `TracerMetadata` object on the heap.
///
/// # Safety
/// This function returns a raw pointer. The caller is responsible for calling
/// `ddog_tracer_metadata_free` to deallocate the memory.
///
/// # Returns
/// A non-null pointer to a newly allocated `TracerMetadata` instance.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_metadata_new() -> *mut TracerMetadata {
    Box::into_raw(Box::new(TracerMetadata::default()))
}

/// Frees a `TracerMetadata` instance previously allocated with `ddog_tracer_metadata_new`.
///
/// # Safety
/// - `ptr` must be a pointer previously returned by `ddog_tracer_metadata_new`.
/// - Double-freeing or passing an invalid pointer results in undefined behavior.
/// - Passing a null pointer is safe and does nothing.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_metadata_free(ptr: *mut TracerMetadata) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(ptr));
    }
}

/// Sets a field of the `TracerMetadata` object pointed to by `ptr`.
///
/// # Arguments
/// - `ptr`: Pointer to a `TracerMetadata` instance.
/// - `kind`: The metadata field to set (as defined in `MetadataKind`).
/// - `value`: A null-terminated C string representing the value to set.
///
/// # Safety
/// - Both `ptr` and `value` must be non-null.
/// - `value` must point to a valid UTF-8 null-terminated string.
/// - If the string is not valid UTF-8, the function does nothing.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_metadata_set(
    ptr: *mut TracerMetadata,
    kind: MetadataKind,
    value: *const c_char,
) {
    if ptr.is_null() || value.is_null() {
        return;
    }

    unsafe {
        let c_str = CStr::from_ptr(value);
        let str_value = match c_str.to_str() {
            Ok(v) => v.to_string(),
            Err(_) => return,
        };

        let metadata = &mut *ptr;

        match kind {
            MetadataKind::RuntimeId => metadata.runtime_id = Some(str_value),
            MetadataKind::TracerLanguage => metadata.tracer_language = str_value,
            MetadataKind::TracerVersion => metadata.tracer_version = str_value,
            MetadataKind::Hostname => metadata.hostname = str_value,
            MetadataKind::ServiceName => metadata.service_name = Some(str_value),
            MetadataKind::ServiceEnv => metadata.service_env = Some(str_value),
            MetadataKind::ServiceVersion => metadata.service_version = Some(str_value),
            MetadataKind::ProcessTags => metadata.process_tags = Some(str_value),
            MetadataKind::ContainerId => metadata.container_id = Some(str_value),
        }
    }
}

/// Serializes the `TracerMetadata` into a platform-specific memory handle (e.g., memfd on Linux).
///
/// # Safety
/// - `ptr` must be a valid, non-null pointer to a `TracerMetadata`.
///
/// # Returns
/// - On Linux: a `TracerMemfdHandle` containing a raw file descriptor to a memory file.
/// - On unsupported platforms: an error.
/// - On failure: propagates any internal errors from the metadata storage process.
///
/// # Platform Support
/// This function currently only supports Linux via `memfd`. On other platforms,
/// it will return an error.
#[no_mangle]
pub unsafe extern "C" fn ddog_tracer_metadata_store(
    ptr: *mut TracerMetadata,
) -> Result<TracerMemfdHandle> {
    if ptr.is_null() {
        return Err::<TracerMemfdHandle, _>(anyhow::anyhow!(
            "Failed to store tracer metadata: received a null pointer"
        ))
        .into();
    }

    let metadata = &mut *ptr;
    let result: anyhow::Result<TracerMemfdHandle> =
        match tracer_metadata::store_tracer_metadata(metadata) {
            #[cfg(target_os = "linux")]
            Ok(handle) => {
                use std::os::fd::{IntoRawFd, OwnedFd};
                let AnonymousFileHandle::Linux(memfd) = handle;
                let owned_fd: OwnedFd = memfd.into_file().into();
                Ok(TracerMemfdHandle {
                    fd: owned_fd.into_raw_fd(),
                })
            }
            #[cfg(not(target_os = "linux"))]
            Ok(_) => Err(anyhow::anyhow!("Unsupported platform")),
            Err(err) => Err(err),
        };
    result.into()
}
