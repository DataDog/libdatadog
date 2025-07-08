// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{c_char, CString};
use std::fmt::Display;

/// Represent error codes that `DDSketchError` struct can hold
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum DDSketchErrorCode {
    InvalidArgument,
    InvalidInput,
    Internal,
}

impl Display for DDSketchErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidArgument => write!(f, "Invalid argument provided"),
            Self::InvalidInput => write!(f, "Invalid input"),
            Self::Internal => write!(f, "Internal error"),
        }
    }
}

/// Structure that contains error information that DDSketch FFI API can return.
#[repr(C)]
#[derive(Debug, PartialEq)]
pub struct DDSketchError {
    pub code: DDSketchErrorCode,
    pub msg: *mut c_char,
}

impl DDSketchError {
    pub fn new(code: DDSketchErrorCode, msg: &str) -> Self {
        Self {
            code,
            msg: CString::new(msg).unwrap_or_default().into_raw(),
        }
    }
}

impl From<Box<dyn std::error::Error>> for DDSketchError {
    fn from(value: Box<dyn std::error::Error>) -> Self {
        DDSketchError::new(DDSketchErrorCode::Internal, &value.to_string())
    }
}

impl Drop for DDSketchError {
    fn drop(&mut self) {
        if !self.msg.is_null() {
            // SAFETY: `the caller must ensure that `DDSketchError` has been created through its
            // `new` method which ensures that `msg` property is originated from
            // `CString::into_raw` call. Any other possibility could lead to UB.
            unsafe {
                drop(CString::from_raw(self.msg));
                self.msg = std::ptr::null_mut();
            }
        }
    }
}

/// Frees `error` and all its contents. After being called error will not point to a valid memory
/// address so any further actions on it could lead to undefined behavior.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_error_free(error: Option<Box<DDSketchError>>) {
    if let Some(error) = error {
        drop(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    #[test]
    fn constructor_test() {
        let code = DDSketchErrorCode::InvalidArgument;
        let error = Box::new(DDSketchError::new(code, &code.to_string()));

        assert_eq!(error.code, DDSketchErrorCode::InvalidArgument);
        let msg = unsafe { CStr::from_ptr(error.msg).to_string_lossy() };
        assert_eq!(msg, DDSketchErrorCode::InvalidArgument.to_string());
    }

    #[test]
    fn destructor_test() {
        let code = DDSketchErrorCode::InvalidArgument;
        let error = Box::new(DDSketchError::new(code, &code.to_string()));

        assert_eq!(error.code, DDSketchErrorCode::InvalidArgument);
        let msg = unsafe { CStr::from_ptr(error.msg).to_string_lossy() };
        assert_eq!(msg, DDSketchErrorCode::InvalidArgument.to_string());

        unsafe { ddog_ddsketch_error_free(Some(error)) };
    }
}
