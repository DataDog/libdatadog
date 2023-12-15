// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

mod receive_report;

use datadog_profiling::crashtracker::*;
use receive_report::receive_report;
use std::{io::prelude::*, time::Duration};

/// Receives data from a crash collector via a pipe on `stdin`, formats it into
/// `CrashInfo` json, and emits it to the endpoint/file defined in `config`.
///
/// At a high-level, this exists because doing anything in a
/// signal handler is dangerous, so we fork a sidecar to do the stuff we aren't
/// allowed to do in the handler.
///
/// See comments in [profiling/crashtracker/mod.rs] for a full architecture
/// description.
pub fn main() -> anyhow::Result<()> {
    let mut config = String::new();
    std::io::stdin().lock().read_line(&mut config)?;
    let config: Configuration = serde_json::from_str(&config)?;

    let mut metadata = String::new();
    std::io::stdin().lock().read_line(&mut metadata)?;
    let metadata: Metadata = serde_json::from_str(&metadata)?;

    match receive_report(&metadata)? {
        receive_report::CrashReportStatus::NoCrash => Ok(()),
        receive_report::CrashReportStatus::CrashReport(mut crash_info) => {
            if config.resolve_frames_in_receiver {
                let ppid = std::os::unix::process::parent_id();
                crash_info.add_names(ppid)?;
            }
            if let Some(path) = config.output_filename {
                crash_info.to_file(&path)?;
            }
            if let Some(endpoint) = config.endpoint {
                // Don't keep the endpoint waiting forever.
                // TODO Experiment to see if 30 is the right number.
                crash_info.upload_to_dd(endpoint, Duration::from_secs(30))?;
            }
            Ok(())
        }
        receive_report::CrashReportStatus::PartialCrashReport(_, _) => todo!(),
    }
}
