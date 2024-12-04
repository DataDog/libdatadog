// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod api;
mod builder;
mod datatypes;
mod metadata;
mod os_info;
mod proc_info;
mod sig_info;
mod span;
mod stackframe;
mod stacktrace;
mod thread_data;

pub use api::*;
pub use builder::*;
pub use datatypes::*;
pub use metadata::*;
pub use os_info::*;
pub use proc_info::*;
pub use sig_info::*;
pub use span::*;
pub use stackframe::*;
pub use stacktrace::*;
pub use thread_data::*;

// /// Best effort attempt to normalize all `ip` on the stacktrace.
// /// `pid` must be the pid of the currently active process where the ips came from.
// ///
// /// # Safety
// /// `crashinfo` must be a valid pointer to a `CrashInfo` object.
// #[cfg(unix)]
// #[no_mangle]
// #[must_use]
// pub unsafe extern "C" fn ddog_crasht_CrashInfo_normalize_ips(
//     crashinfo: *mut CrashInfo,
//     pid: u32,
// ) -> VoidResult {
//     (|| {
//         let crashinfo = crashinfo_ptr_to_inner(crashinfo)?;
//         crashinfo.normalize_ips(pid)
//     })()
//     .context("ddog_crasht_CrashInfo_normalize_ips failed")
//     .into()
// }
