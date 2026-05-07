// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C ABI for [`libdd_http_client::MultipartPart`].
//!
//! Multipart parts are owned by C as opaque `Box<MultipartPart>` pointers.
//! They are populated through builder-style setters and ultimately handed
//! off to a request via
//! [`crate::ddog_http_request_with_multipart_part`], which consumes the
//! part. If the caller decides to abandon a part before attaching it, it
//! must be released via [`ddog_multipart_part_drop`].

use crate::error::DdogHttpClientError;
use libdd_common_ffi::slice::{AsBytes, ByteSlice};
use libdd_common_ffi::CharSlice;
use libdd_http_client::MultipartPart;
use std::ptr::NonNull;

/// Allocate a new [`MultipartPart`] with the given form-field name and
/// raw bytes.
///
/// `name` must be valid UTF-8. `data` is copied into the part (any byte
/// sequence; not required to be UTF-8). The new part is written into
/// `*out_handle` and owned by the caller.
///
/// # Safety
/// `name` must point to valid memory for its declared length and contain
/// valid UTF-8. `data` must point to valid memory for its declared length.
/// `out_handle` must be a valid, writable pointer to an uninitialised
/// `*mut ddog_MultipartPart`.
#[no_mangle]
pub unsafe extern "C" fn ddog_multipart_part_new(
    name: CharSlice,
    data: ByteSlice,
    out_handle: NonNull<Box<MultipartPart>>,
) -> Option<Box<DdogHttpClientError>> {
    let name_str = match name.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogHttpClientError::invalid_argument(&format!(
                "multipart part name is not valid UTF-8: {e}"
            ))))
        }
    };
    let bytes = bytes::Bytes::copy_from_slice(data.as_bytes());
    let part = MultipartPart::new(name_str, bytes);
    out_handle.as_ptr().write(Box::new(part));
    None
}

/// Set the filename associated with this multipart part.
///
/// `filename` must be valid UTF-8. Calling this more than once replaces
/// the previous filename.
///
/// # Safety
/// `part` must be `None` or a valid mutable reference to a part produced
/// by [`ddog_multipart_part_new`]. `filename` must point to valid memory
/// for its declared length.
#[no_mangle]
pub unsafe extern "C" fn ddog_multipart_part_with_filename(
    part: Option<&mut MultipartPart>,
    filename: CharSlice,
) -> Option<Box<DdogHttpClientError>> {
    let Some(p) = part else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "multipart part is null",
        )));
    };
    let filename_str = match filename.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogHttpClientError::invalid_argument(&format!(
                "multipart part filename is not valid UTF-8: {e}"
            ))))
        }
    };
    // MultipartPart's setters take `self`, so swap-replace via a sentinel.
    let taken = std::mem::replace(p, MultipartPart::new("", bytes::Bytes::new()));
    *p = taken.with_filename(filename_str);
    None
}

/// Set the MIME content type for this multipart part (e.g.
/// `"application/json"`, `"text/plain"`).
///
/// `content_type` must be valid UTF-8. Calling this more than once
/// replaces the previous content type.
///
/// # Safety
/// `part` must be `None` or a valid mutable reference to a part produced
/// by [`ddog_multipart_part_new`]. `content_type` must point to valid
/// memory for its declared length.
#[no_mangle]
pub unsafe extern "C" fn ddog_multipart_part_with_content_type(
    part: Option<&mut MultipartPart>,
    content_type: CharSlice,
) -> Option<Box<DdogHttpClientError>> {
    let Some(p) = part else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "multipart part is null",
        )));
    };
    let ct_str = match content_type.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogHttpClientError::invalid_argument(&format!(
                "multipart part content_type is not valid UTF-8: {e}"
            ))))
        }
    };
    let taken = std::mem::replace(p, MultipartPart::new("", bytes::Bytes::new()));
    *p = taken.with_content_type(ct_str);
    None
}

/// Drop a multipart part that was not attached to a request.
///
/// # Safety
/// `part` must be `None` or a part produced by
/// [`ddog_multipart_part_new`] and not yet consumed by
/// [`crate::ddog_http_request_with_multipart_part`].
#[no_mangle]
pub unsafe extern "C" fn ddog_multipart_part_drop(part: Option<Box<MultipartPart>>) {
    drop(part)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    fn cs(s: &str) -> CharSlice<'_> {
        CharSlice::from(s)
    }

    #[test]
    fn new_part_defaults() {
        unsafe {
            let mut part: MaybeUninit<Box<MultipartPart>> = MaybeUninit::uninit();
            let err = ddog_multipart_part_new(
                cs("file"),
                ByteSlice::from(b"hello".as_slice()),
                NonNull::new_unchecked(part.as_mut_ptr()),
            );
            assert!(err.is_none());
            let part = part.assume_init();
            assert_eq!(part.name(), "file");
            assert_eq!(part.data().as_ref(), b"hello");
            assert!(part.filename().is_none());
            assert!(part.content_type().is_none());
            ddog_multipart_part_drop(Some(part));
        }
    }

    #[test]
    fn with_filename_and_content_type() {
        unsafe {
            let mut part: MaybeUninit<Box<MultipartPart>> = MaybeUninit::uninit();
            let _ = ddog_multipart_part_new(
                cs("file"),
                ByteSlice::from(b"x".as_slice()),
                NonNull::new_unchecked(part.as_mut_ptr()),
            );
            let mut part = part.assume_init();

            let err = ddog_multipart_part_with_filename(Some(&mut part), cs("a.txt"));
            assert!(err.is_none());
            let err = ddog_multipart_part_with_content_type(Some(&mut part), cs("text/plain"));
            assert!(err.is_none());

            assert_eq!(part.filename(), Some("a.txt"));
            assert_eq!(part.content_type(), Some("text/plain"));
            ddog_multipart_part_drop(Some(part));
        }
    }

    #[test]
    fn null_part_returns_invalid_argument() {
        unsafe {
            let err = ddog_multipart_part_with_filename(None, cs("a.txt"));
            assert!(err.is_some());
            assert_eq!(
                err.unwrap().code,
                crate::DdogHttpClientErrorCode::InvalidArgument
            );
            let err = ddog_multipart_part_with_content_type(None, cs("text/plain"));
            assert!(err.is_some());
        }
    }

    #[test]
    fn drop_handles_none() {
        unsafe { ddog_multipart_part_drop(None) };
    }
}
