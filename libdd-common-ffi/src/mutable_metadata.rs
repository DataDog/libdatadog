// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! FFI bindings for the shared, updatable tracer metadata handle.
//!
//! A `ddog_MutableMetadataHandle` is created with [`ddog_mutable_metadata_new`], updated
//! through [`ddog_mutable_metadata_set_runtime_id`] /
//! [`ddog_mutable_metadata_set_process_tags`], and freed with
//! [`ddog_mutable_metadata_free`]. See the `libdd_common::mutable_metadata` module
//! documentation for the read/write protocol.

use ::function_name::named;
use core::ptr::NonNull;

use libdd_common::mutable_metadata::{InstrumentationSession, MutableMetadataHandle};

use crate::slice::{AsBytes, CharSlice};
use crate::{Error, VoidResult, wrap_with_void_ffi_result};

/// Creates a shared, updatable metadata handle initialized with default values.
///
/// Use the metadata setters to configure or update its values.
///
/// # Safety
///
/// `out_handle` must point to valid, writable (uninitialized) memory for a
/// `ddog_MutableMetadataHandle *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_mutable_metadata_new(
    out_handle: NonNull<Box<MutableMetadataHandle>>,
) {
    // SAFETY: `out_handle` points to valid, writable memory per this function's safety contract.
    unsafe {
        out_handle
            .as_ptr()
            .write(Box::new(MutableMetadataHandle::default()));
    }
}

/// Frees a `ddog_MutableMetadataHandle` handle.
///
/// Call once this handle is no longer needed. It must not be used concurrently
/// with this call or accessed afterward. Other cloned handles remain valid.
///
/// # Safety
///
/// `handle` must be a valid mutable metadata handle obtained through [`ddog_mutable_metadata_new`]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_mutable_metadata_free(handle: Box<MutableMetadataHandle>) {
    drop(handle);
}

/// Replaces the `runtime_id` held by the shared mutable metadata handle.
///
/// # Safety
///
/// `handle` must be a valid mutable metadata handle obtained through [`ddog_mutable_metadata_new`]
#[must_use]
#[named]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_mutable_metadata_set_runtime_id(
    handle: Option<&MutableMetadataHandle>,
    runtime_id: CharSlice,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        let handle = handle.ok_or_else(|| Error::from("Invalid handle"))?;
        let runtime_id = runtime_id.to_utf8_lossy().into_owned();
        handle.set_runtime_id(runtime_id);
    })
}

/// Replaces the `process_tags` held by the shared mutable metadata handle.
///
/// # Safety
///
/// `handle` must be a valid mutable metadata handle obtained through [`ddog_mutable_metadata_new`]
#[must_use]
#[named]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_mutable_metadata_set_process_tags(
    handle: Option<&MutableMetadataHandle>,
    process_tags: CharSlice,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        let handle = handle.ok_or_else(|| Error::from("Invalid handle"))?;
        let process_tags = process_tags.to_utf8_lossy().into_owned();
        handle.set_process_tags(process_tags);
    })
}

/// Atomically replaces runtime identity and instrumentation session ancestry.
/// Empty session strings omit the corresponding headers, overriding static configuration.
/// Strings are copied before returning; process tags are preserved.
///
/// For a fork child, call after all native workers and sends have paused and before
/// `ddog_shared_runtime_after_fork_child`. This does not reset pending data or sequences.
///
/// # Safety
///
/// `handle` must be valid and all slices must remain valid for the duration of the call.
#[must_use]
#[named]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_mutable_metadata_set_identity(
    handle: Option<&MutableMetadataHandle>,
    runtime_id: CharSlice,
    session_id: CharSlice,
    root_session_id: CharSlice,
    parent_session_id: CharSlice,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        let handle = handle.ok_or_else(|| Error::from("Invalid handle"))?;
        let runtime_id = runtime_id.try_to_utf8()?.to_owned();
        let session = InstrumentationSession {
            id: session_id.try_to_utf8()?.to_owned(),
            root_id: root_session_id.try_to_utf8()?.to_owned(),
            parent_id: parent_session_id.try_to_utf8()?.to_owned(),
        };
        handle.set_identity(runtime_id, session);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    fn new_handle() -> Box<MutableMetadataHandle> {
        let mut handle: core::mem::MaybeUninit<Box<MutableMetadataHandle>> =
            core::mem::MaybeUninit::uninit();
        // Safety: `handle` is valid, writable memory for the boxed handle.
        unsafe {
            ddog_mutable_metadata_new(NonNull::new_unchecked(&mut handle).cast());
            handle.assume_init()
        }
    }

    #[test]
    fn new_yields_default_values() {
        let handle = new_handle();
        let snapshot = handle.load();
        assert_eq!(snapshot.runtime_id, "");
        assert_eq!(snapshot.process_tags, "");
    }

    #[test]
    fn setters_update_their_field_only() {
        let handle = new_handle();

        // Safety: the handle is valid and not shared.
        unsafe {
            assert!(matches!(
                ddog_mutable_metadata_set_runtime_id(
                    Some(handle.as_ref()),
                    CharSlice::from("rt-1")
                ),
                VoidResult::Ok
            ));
            assert!(matches!(
                ddog_mutable_metadata_set_process_tags(
                    Some(handle.as_ref()),
                    CharSlice::from("key1:val1,key2:val2"),
                ),
                VoidResult::Ok
            ));
            let snapshot = handle.load();
            assert_eq!(snapshot.runtime_id, "rt-1");
            assert_eq!(snapshot.process_tags, "key1:val1,key2:val2");

            // Field updates only touch their field.
            assert!(matches!(
                ddog_mutable_metadata_set_runtime_id(
                    Some(handle.as_ref()),
                    CharSlice::from("rt-2")
                ),
                VoidResult::Ok
            ));
            let snapshot = handle.load();
            assert_eq!(snapshot.runtime_id, "rt-2");
            assert_eq!(snapshot.process_tags, "key1:val1,key2:val2");
        }
    }

    #[test]
    fn invalid_utf8_is_replaced_lossily() {
        let handle = new_handle();

        // Safety: the handle is valid and not shared.
        unsafe {
            let invalid: [u8; 2] = [0x80u8, 0xFFu8];
            assert!(matches!(
                ddog_mutable_metadata_set_runtime_id(
                    Some(handle.as_ref()),
                    CharSlice::from_bytes(&invalid),
                ),
                VoidResult::Ok
            ));
            let snapshot = handle.load();
            assert_eq!(snapshot.runtime_id, "\u{FFFD}\u{FFFD}");
        }
    }

    #[test]
    fn null_handle_is_an_error() {
        // Safety: the handles are null; the functions must reject them.
        unsafe {
            let result = ddog_mutable_metadata_set_runtime_id(None, CharSlice::from("x"));
            let error = result.unwrap_err();
            assert_eq!(
                error.to_string(),
                "ddog_mutable_metadata_set_runtime_id failed: Invalid handle"
            );

            let result = ddog_mutable_metadata_set_process_tags(None, CharSlice::from("k:v"));
            let error = result.unwrap_err();
            assert_eq!(
                error.to_string(),
                "ddog_mutable_metadata_set_process_tags failed: Invalid handle"
            );
        }
    }

    #[test]
    fn updates_propagate_to_cloned_handles() {
        let handle = new_handle();
        let clone = handle.clone();

        // Safety: both handles are valid.
        unsafe {
            assert!(matches!(
                ddog_mutable_metadata_set_runtime_id(Some(clone.as_ref()), CharSlice::from("rt-1")),
                VoidResult::Ok
            ));
            let snapshot = handle.load();
            assert_eq!(snapshot.runtime_id, "rt-1");
        }
    }

    #[test]
    fn identity_validation_is_atomic() {
        let handle = new_handle();
        handle.set_process_tags("key:value".into());
        // Safety: the handle and all slices are valid for each call.
        unsafe {
            assert!(matches!(
                ddog_mutable_metadata_set_identity(
                    Some(&handle),
                    CharSlice::from("child"),
                    CharSlice::from("session"),
                    CharSlice::from("root"),
                    CharSlice::from("parent"),
                ),
                VoidResult::Ok
            ));
            let before = handle.load_full();
            assert_eq!(before.runtime_id, "child");
            assert_eq!(before.process_tags, "key:value");
            assert_eq!(before.session.as_ref().unwrap().parent_id, "parent");
            let result = ddog_mutable_metadata_set_identity(
                Some(&handle),
                CharSlice::from("must-not-be-published"),
                CharSlice::from("session"),
                CharSlice::from("root"),
                CharSlice::from_bytes(&[0xff]),
            );
            assert!(matches!(result, VoidResult::Err(_)));
            assert_eq!(*before, **handle.load());
            assert!(matches!(
                ddog_mutable_metadata_set_identity(
                    None,
                    CharSlice::empty(),
                    CharSlice::empty(),
                    CharSlice::empty(),
                    CharSlice::empty(),
                ),
                VoidResult::Err(_)
            ));
        }
    }
}
