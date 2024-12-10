// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ::function_name::named;
use ddcommon_ffi::{slice::AsBytes, wrap_with_void_ffi_result, CharSlice, VoidResult};
#[no_mangle]
#[must_use]
#[named]
/// Receives data from a crash collector via a pipe on `stdin`, formats it into
/// `CrashInfo` json, and emits it to the endpoint/file defined in `config`.
///
/// At a high-level, this exists because doing anything in a
/// signal handler is dangerous, so we fork a sidecar to do the stuff we aren't
/// allowed to do in the handler.
///
/// See comments in [crashtracker/lib.rs] for a full architecture description.
/// # Safety
/// No safety concerns
pub unsafe extern "C" fn ddog_crasht_receiver_entry_point_stdin() -> VoidResult {
    wrap_with_void_ffi_result!({ datadog_crashtracker::receiver_entry_point_stdin()? })
}

#[no_mangle]
#[must_use]
#[named]
/// Receives data from a crash collector via a pipe on `stdin`, formats it into
/// `CrashInfo` json, and emits it to the endpoint/file defined in `config`.
///
/// At a high-level, this exists because doing anything in a
/// signal handler is dangerous, so we fork a sidecar to do the stuff we aren't
/// allowed to do in the handler.
///
/// See comments in [profiling/crashtracker/mod.rs] for a full architecture
/// description.
/// # Safety
/// No safety concerns
pub unsafe extern "C" fn ddog_crasht_receiver_entry_point_unix_socket(
    socket_path: CharSlice,
) -> VoidResult {
    wrap_with_void_ffi_result!({
        datadog_crashtracker::receiver_entry_point_unix_socket(socket_path.try_to_utf8()?)?
    })
}
