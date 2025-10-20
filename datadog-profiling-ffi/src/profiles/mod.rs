// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod pprof_builder;
mod profile;
mod profiling_dictionary;
mod sample_builder;
mod scratchpad;
mod utf8;

pub use pprof_builder::*;
pub use profile::*;
pub use profiling_dictionary::*;
pub use sample_builder::*;
pub use scratchpad::*;
pub use utf8::*;

use std::ffi::CStr;

// Shared error message helpers and null-check macros reused by FFI modules.
pub const fn null_out_param_err() -> &'static CStr {
    c"null pointer used as out parameter"
}

pub const fn null_insert_err() -> &'static CStr {
    c"tried to insert a null pointer"
}

#[macro_export]
macro_rules! ensure_non_null_out_parameter {
    ($expr:expr) => {
        if $expr.is_null() {
            return $crate::ProfileStatus::from(
                $crate::profiles::null_out_param_err(),
            );
        }
    };
}

#[macro_export]
macro_rules! ensure_non_null_insert {
    ($expr:expr) => {
        if $expr.is_null() {
            return $crate::ProfileStatus::from(
                $crate::profiles::null_insert_err(),
            );
        }
    };
}

pub(crate) use {ensure_non_null_insert, ensure_non_null_out_parameter};
