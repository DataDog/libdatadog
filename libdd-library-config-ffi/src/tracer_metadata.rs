// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::ffi::CStr;
#[cfg(all(feature = "catch_panic", panic = "unwind"))]
use libdd_common_ffi::wrap_with_ffi_result;
#[cfg(not(all(feature = "catch_panic", panic = "unwind")))]
use libdd_common_ffi::wrap_with_ffi_result_no_catch as wrap_with_ffi_result;
use libdd_common_ffi::{Result, VoidResult};
#[cfg(target_os = "linux")]
use libdd_library_config::tracer_metadata::AnonymousFileHandle;
use libdd_library_config::tracer_metadata::{self, TracerMetadata};
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_tracer_metadata_new() -> *mut TracerMetadata {
    Box::into_raw(Box::new(TracerMetadata::default()))
}

/// Frees a `TracerMetadata` instance previously allocated with `ddog_tracer_metadata_new`.
///
/// # Safety
/// - `ptr` must be a pointer previously returned by `ddog_tracer_metadata_new`.
/// - Double-freeing or passing an invalid pointer results in undefined behavior.
/// - Passing a null pointer is safe and does nothing.
#[unsafe(no_mangle)]
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
#[unsafe(no_mangle)]
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

/// Includes thread-context discovery metadata in the `TracerMetadata` object pointed to by `ptr`.
///
/// If the builder has no thread-context metadata, configures it to publish
/// the `tlsdesc_v1_dev` schema and a key map containing `datadog.local_root_span_id`.
/// If the thread-context metadata is already set, leaves it unchanged.
///
/// This function only updates the builder. Call `ddog_tracer_metadata_store` to publish it.
///
/// # Arguments
/// - `ptr`: Pointer to a `TracerMetadata` instance.
///
/// # Safety
/// - If non-null, `ptr` must point to a valid, properly aligned, live `TracerMetadata`.
/// - The caller must ensure exclusive access to the instance for the duration of the call.
/// - Ownership remains with the caller
///
/// # Returns
/// - On success: `VoidResult::Ok`, also when thread-context metadata is already present
/// - An error if `ptr` is null.
#[unsafe(no_mangle)]
#[function_name::named]
pub unsafe extern "C" fn ddog_tracer_metadata_include_otel_thread_context(
    ptr: *mut TracerMetadata,
) -> VoidResult {
    wrap_with_ffi_result!({
        let metadata = unsafe { ptr.as_mut() }.ok_or_else(|| {
            anyhow::anyhow!("Failed to include OTel thread context: received a null pointer")
        })?;

        metadata
            .threadlocal_metadata
            .get_or_insert_with(Default::default);

        anyhow::Ok(())
    })
}

/// Serializes the `TracerMetadata` into a platform-specific memory handle (e.g., memfd on Linux).
/// This function also attempts to publish the tracer metadata as an OTel process context
/// separately, but will ignore resulting errors.
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_tracer_metadata_store(
    ptr: *mut TracerMetadata,
) -> Result<TracerMemfdHandle> {
    unsafe {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_library_config::tracer_metadata::ThreadLocalMetadata;

    #[test]
    fn include_otel_thread_context_rejects_null() {
        let result =
            unsafe { ddog_tracer_metadata_include_otel_thread_context(core::ptr::null_mut()) };

        let error = result.unwrap_err();
        assert!(error.to_string().contains("null pointer"), "{error}");
    }

    #[test]
    fn include_otel_thread_context_sets_defaults_and_is_idempotent() {
        unsafe {
            let ptr = ddog_tracer_metadata_new();

            assert!((*ptr).threadlocal_metadata.is_none());
            (*ptr).service_name = Some("some-service".to_owned());

            let expected = TracerMetadata {
                service_name: Some("some-service".to_owned()),
                threadlocal_metadata: Some(Default::default()),
                ..Default::default()
            };

            for _ in 0..2 {
                ddog_tracer_metadata_include_otel_thread_context(ptr).unwrap();
                assert_eq!(&*ptr, &expected);
            }

            ddog_tracer_metadata_free(ptr);
        }
    }

    #[test]
    fn include_otel_thread_context_preserves_existing_metadata() {
        unsafe {
            let ptr = ddog_tracer_metadata_new();

            (*ptr).threadlocal_metadata = Some(ThreadLocalMetadata {
                attribute_keys: vec!["some.key".to_owned()],
                schema_version: Some("some_schema".to_owned()),
                ..Default::default()
            });

            ddog_tracer_metadata_include_otel_thread_context(ptr).unwrap();

            let metadata = (*ptr)
                .threadlocal_metadata
                .as_ref()
                .expect("thread-context metadata should remain present");

            assert_eq!(metadata.attribute_keys, vec!["some.key".to_owned()]);
            assert_eq!(metadata.schema_version.as_deref(), Some("some_schema"));

            ddog_tracer_metadata_free(ptr);
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn thread_context_metadata_publication_lifecycle() {
        use libdd_library_config::otel_process_ctx::{ProcessContextSelfReader, unpublish};
        use std::os::fd::{FromRawFd, OwnedFd};

        unsafe {
            let ptr = ddog_tracer_metadata_new();
            (*ptr).service_name = Some("some-service".to_owned());

            let expected = (*ptr).to_otel_process_ctx();

            let handle = ddog_tracer_metadata_store(ptr).unwrap();
            let legacy_fd = OwnedFd::from_raw_fd(handle.fd);

            let reader =
                ProcessContextSelfReader::new().expect("published context should be discoverable");

            let actual = reader.read().expect("published context should be readable");

            assert_eq!(actual, expected);

            ddog_tracer_metadata_include_otel_thread_context(ptr).unwrap();

            let expected_with_thread_context = (*ptr).to_otel_process_ctx();
            assert_ne!(expected_with_thread_context, expected);

            let before_store = reader
                .read()
                .expect("previous publication should remain readable");
            assert_eq!(before_store, expected);

            let updated_handle = ddog_tracer_metadata_store(ptr).unwrap();
            drop(OwnedFd::from_raw_fd(updated_handle.fd));

            let after_store = reader
                .read()
                .expect("updated publication should be readable");
            assert_eq!(after_store, expected_with_thread_context);

            let replacement = ddog_tracer_metadata_new();
            (*replacement).service_name = Some("updated-service".to_owned());

            ddog_tracer_metadata_include_otel_thread_context(replacement).unwrap();
            assert!((*replacement).threadlocal_metadata.is_some());

            let expected_replacement = (*replacement).to_otel_process_ctx();
            assert_ne!(expected_replacement, expected_with_thread_context);

            let replacement_handle = ddog_tracer_metadata_store(replacement).unwrap();
            drop(OwnedFd::from_raw_fd(replacement_handle.fd));

            ddog_tracer_metadata_free(ptr);
            ddog_tracer_metadata_free(replacement);
            drop(legacy_fd);

            let after_cleanup = reader
                .read()
                .expect("replacement publication should remain readable after cleanup");
            assert_eq!(after_cleanup, expected_replacement);

            unpublish().expect("published context should be removed");
        }
    }

    #[cfg(all(feature = "catch_panic", panic = "unwind"))]
    #[test]
    #[function_name::named]
    fn tracer_metadata_result_converts_panic_to_error() {
        let operation = || -> anyhow::Result<()> { panic!("test panic") };

        let result: VoidResult = wrap_with_ffi_result!({ operation() });
        let message = result.unwrap_err().to_string();

        assert!(message.contains(function_name!()), "{message}");
        assert!(message.contains("test panic"), "{message}");
    }

    #[cfg(all(not(feature = "catch_panic"), panic = "unwind"))]
    #[test]
    #[function_name::named]
    fn tracer_metadata_result_propagates_panic_without_containment() {
        let operation = || -> anyhow::Result<()> { panic!("test panic") };

        let result =
            std::panic::catch_unwind(|| -> VoidResult { wrap_with_ffi_result!({ operation() }) });

        assert!(result.is_err());
    }
}
