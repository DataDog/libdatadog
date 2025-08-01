// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon_ffi::CString;
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
#[derive(Debug)]
pub struct DDSketchError {
    pub code: DDSketchErrorCode,
    pub msg: CString,
}

impl DDSketchError {
    pub fn new(code: DDSketchErrorCode, msg: &str) -> Self {
        Self {
            code,
            msg: CString::new_or_empty(msg),
        }
    }
}

impl From<Box<dyn std::error::Error>> for DDSketchError {
    fn from(value: Box<dyn std::error::Error>) -> Self {
        DDSketchError::new(DDSketchErrorCode::Internal, &value.to_string())
    }
}

/// Frees `error` and all its contents. After being called error will not point to a valid memory
/// address so any further actions on it could lead to undefined behavior.
///
/// # Safety
///
/// Only pass null or a pointer to a valid DDSketchError created by this library.
#[no_mangle]
pub unsafe extern "C" fn ddog_ddsketch_error_free(error: Option<Box<DDSketchError>>) {
    drop(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructor_test() {
        let code = DDSketchErrorCode::InvalidArgument;
        let error = Box::new(DDSketchError::new(code, &code.to_string()));

        assert_eq!(error.code, DDSketchErrorCode::InvalidArgument);
        let msg = error.msg.as_cstr().into_std().to_str().unwrap();
        assert_eq!(msg, DDSketchErrorCode::InvalidArgument.to_string());
    }

    #[test]
    fn destructor_test() {
        let code = DDSketchErrorCode::InvalidArgument;
        let error = Box::new(DDSketchError::new(code, &code.to_string()));

        assert_eq!(error.code, DDSketchErrorCode::InvalidArgument);
        let msg = error.msg.as_cstr().into_std().to_str().unwrap();
        assert_eq!(msg, DDSketchErrorCode::InvalidArgument.to_string());

        unsafe { ddog_ddsketch_error_free(Some(error)) };
    }

    #[test]
    fn test_error_with_null_bytes() {
        let code = DDSketchErrorCode::InvalidInput;
        let error = Box::new(DDSketchError::new(code, "Error with\0null bytes"));

        assert_eq!(error.code, DDSketchErrorCode::InvalidInput);
        let msg = error.msg.as_cstr().into_std().to_str().unwrap();
        assert_eq!(msg, ""); // Should fall back to empty string

        unsafe { ddog_ddsketch_error_free(Some(error)) };
    }
}
