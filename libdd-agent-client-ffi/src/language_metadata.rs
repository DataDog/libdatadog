// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C ABI for [`libdd_agent_client::LanguageMetadata`].
//!
//! Language metadata is owned by C as an opaque `Box<LanguageMetadata>`
//! pointer and is consumed by
//! [`crate::ddog_agent_client_builder_set_language_metadata`]. If the
//! caller decides to abandon the metadata before attaching it, it must
//! be released via [`ddog_language_metadata_drop`].

use crate::error::DdogAgentClientError;
use libdd_agent_client::LanguageMetadata;
use libdd_common_ffi::slice::AsBytes;
use libdd_common_ffi::CharSlice;
use std::ptr::NonNull;

/// Allocate a new [`LanguageMetadata`].
///
/// All four arguments must be valid UTF-8.
///
/// - `language`: e.g. `"python"`, `"ruby"`.
/// - `language_version`: e.g. `"3.12.1"`.
/// - `language_interpreter`: e.g. `"CPython"`, `"MRI"`.
/// - `tracer_version`: e.g. `"2.18.0"`.
///
/// On success writes a `Box<LanguageMetadata>` into `*out_handle` and
/// returns `None`. On failure returns the error and leaves
/// `*out_handle` unchanged.
///
/// # Safety
/// All `CharSlice` arguments must point to valid memory for their
/// declared lengths. `out_handle` must be a valid, writable pointer to
/// an uninitialised `*mut ddog_LanguageMetadata`.
#[no_mangle]
pub unsafe extern "C" fn ddog_language_metadata_new(
    language: CharSlice,
    language_version: CharSlice,
    language_interpreter: CharSlice,
    tracer_version: CharSlice,
    out_handle: NonNull<Box<LanguageMetadata>>,
) -> Option<Box<DdogAgentClientError>> {
    let language = match language.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogAgentClientError::invalid_argument(&format!(
                "language is not valid UTF-8: {e}"
            ))))
        }
    };
    let language_version = match language_version.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogAgentClientError::invalid_argument(&format!(
                "language_version is not valid UTF-8: {e}"
            ))))
        }
    };
    let language_interpreter = match language_interpreter.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogAgentClientError::invalid_argument(&format!(
                "language_interpreter is not valid UTF-8: {e}"
            ))))
        }
    };
    let tracer_version = match tracer_version.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogAgentClientError::invalid_argument(&format!(
                "tracer_version is not valid UTF-8: {e}"
            ))))
        }
    };
    let metadata = LanguageMetadata::new(
        language,
        language_version,
        language_interpreter,
        tracer_version,
    );
    out_handle.as_ptr().write(Box::new(metadata));
    None
}

/// Drop a [`LanguageMetadata`] that was not attached to a builder.
///
/// # Safety
/// `metadata` must be `None` or a metadata produced by
/// [`ddog_language_metadata_new`] and not yet consumed by
/// [`crate::ddog_agent_client_builder_set_language_metadata`].
#[no_mangle]
pub unsafe extern "C" fn ddog_language_metadata_drop(
    metadata: Option<Box<LanguageMetadata>>,
) {
    drop(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    fn cs(s: &str) -> CharSlice<'_> {
        CharSlice::from(s)
    }

    #[test]
    fn new_round_trip() {
        unsafe {
            let mut handle: MaybeUninit<Box<LanguageMetadata>> = MaybeUninit::uninit();
            let err = ddog_language_metadata_new(
                cs("python"),
                cs("3.12.1"),
                cs("CPython"),
                cs("2.18.0"),
                NonNull::new_unchecked(handle.as_mut_ptr()),
            );
            assert!(err.is_none());
            let metadata = handle.assume_init();
            assert_eq!(metadata.language, "python");
            assert_eq!(metadata.language_version, "3.12.1");
            assert_eq!(metadata.interpreter, "CPython");
            assert_eq!(metadata.tracer_version, "2.18.0");
            ddog_language_metadata_drop(Some(metadata));
        }
    }

    #[test]
    fn drop_handles_none() {
        unsafe { ddog_language_metadata_drop(None) };
    }
}
