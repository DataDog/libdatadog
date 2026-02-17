// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::counters::get_counters;
use super::crash_handler;
use crate::{CrashInfoBuilder, ErrorKind, ProcInfo, StackTrace};

pub fn report_unhandled_exception(
    error_type: Option<&str>,
    error_message: Option<&str>,
    stack: StackTrace,
) -> anyhow::Result<()> {
    // If crash tracker has not been initialized, return
    let Some(metadata) = crash_handler::get_metadata() else {
        return Ok(());
    };
    let Some(config) = crash_handler::get_config() else {
        return Ok(());
    };

    let mut builder = CrashInfoBuilder::new();
    builder.with_kind(ErrorKind::UnhandledException)?;

    let error_type_str = error_type.unwrap_or("<unknown>");
    let error_message_str = error_message.unwrap_or("<no message>");
    let message = format!(
        "Process was terminated due to an unhandled exception of type '{error_type_str}'. \
         Message: \"{error_message_str}\""
    );

    builder.with_message(message)?;
    builder.with_metadata(metadata)?;

    let crash_ping = builder.build_crash_ping()?;
    crash_ping.upload_to_endpoint(config.endpoint())?;

    builder.with_stack(stack)?;
    builder.with_os_info_this_machine()?;
    if let Ok(counters) = get_counters() {
        builder.with_counters(counters)?;
    }

    builder.with_proc_info(ProcInfo {
        pid: unsafe { libc::getpid() } as u32,
        tid: None,
    })?;

    builder.with_timestamp_now()?;

    let crash_info = builder.build()?;
    crash_info.upload_to_endpoint(config.endpoint())?;

    Ok(())
}
