// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod compressor;

pub use datadog_profiling::profiles::ProfileError;

use ddcommon_ffi::CharSlice;

/// Returns a short description for the error. The message is a static string
/// and doesn't need any free/dtor/drop. It is also guaranteed to a valid
/// nul terminated string with no interior null bytes (C-string).
#[no_mangle]
#[must_use]
#[cold]
pub extern "C" fn ddog_prof_Profile_Error_message(
    err: ProfileError,
) -> CharSlice<'static> {
    CharSlice::from(err.as_str())
}
