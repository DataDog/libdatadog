// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod compressor;
// mod datatypes;
// mod interning_api;
mod endpoints;
mod ffi_stores;
mod labels_set;
mod profile_builder;
mod samples;
mod stack_trace_set;
mod string_table;

pub use compressor::*;
pub use endpoints::*;
pub use ffi_stores::*;
pub use labels_set::*;
pub use profile_builder::*;
pub use samples::*;
pub use string_table::*;

pub use datadog_profiling::profiles::ProfileError;

use ddcommon_ffi::CharSlice;

/// Returns a short description for the error. The message is a static string
/// and doesn't need any free/dtor/drop.
#[no_mangle]
#[must_use]
#[cold]
pub extern "C" fn ddog_prof_Profile_Error_message(
    err: ProfileError,
) -> CharSlice<'static> {
    CharSlice::from(err.as_str())
}
