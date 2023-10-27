// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

mod experiments;
mod exporters;
mod recieve_report;

use datadog_profiling::crashtracker::*;
use exporters::{_print_to_file, upload_to_dd};
use recieve_report::receive_report;
use std::io::prelude::*;

/// Recieves data on stdin, and forwards it to somewhere its useful
/// For now, just sent to a file.
/// Future enhancement: set of key/value pairs sent over pipe to setup
/// Future enhancement: publish to DD endpoint
pub fn main() -> anyhow::Result<()> {
    let mut config = String::new();
    std::io::stdin().lock().read_line(&mut config)?;
    let config: Configuration = serde_json::from_str(&config)?;

    let mut metadata = String::new();
    std::io::stdin().lock().read_line(&mut metadata)?;
    let metadata: Metadata = serde_json::from_str(&metadata)?;

    match receive_report(&metadata)? {
        recieve_report::CrashReportStatus::NoCrash => Ok(()),
        recieve_report::CrashReportStatus::CrashReport(buf) => {
            _print_to_file(&buf)?;
            upload_to_dd(&buf, &config, &metadata)?;
            Ok(())
        }
        recieve_report::CrashReportStatus::PartialCrashReport(_) => todo!(),
    }
}
