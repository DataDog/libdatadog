// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::crashtracker::datatypes::*;
use anyhow::Context;
use ddcommon_ffi::{slice::AsBytes, CharSlice};
#[no_mangle]
#[must_use]
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
pub unsafe extern "C" fn ddog_prof_Crashtracker_receiver_entry_point_stdin() -> CrashtrackerResult {
    datadog_crashtracker::receiver_entry_point_stdin()
        .context("ddog_prof_Crashtracker_receiver_entry_point_stdin failed")
        .into()
}

#[no_mangle]
#[must_use]
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
pub unsafe extern "C" fn ddog_prof_Crashtracker_receiver_entry_point_unix_socket(
    socket_path: CharSlice,
) -> CrashtrackerResult {
    (|| {
        let socket_path = socket_path.try_to_utf8()?;
        datadog_crashtracker::reciever_entry_point_unix_socket(socket_path)
    })()
    .context("ddog_prof_Crashtracker_receiver_entry_point_unix_socket failed")
    .into()
}
