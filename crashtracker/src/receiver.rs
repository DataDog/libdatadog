// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::*;
use anyhow::Context;
use std::{io::BufRead, time::Duration};

/// Receives data from a crash collector via a pipe on `stdin`, formats it into
/// `CrashInfo` json, and emits it to the endpoint/file defined in `config`.
///
/// At a high-level, this exists because doing anything in a
/// signal handler is dangerous, so we fork a sidecar to do the stuff we aren't
/// allowed to do in the handler.
///
/// See comments in [profiling/crashtracker/mod.rs] for a full architecture
/// description.
pub fn receiver_entry_point() -> anyhow::Result<()> {
    let mut config = String::new();
    std::io::stdin().lock().read_line(&mut config)?;
    let config: CrashtrackerConfiguration = serde_json::from_str(&config)?;

    let mut metadata = String::new();
    std::io::stdin().lock().read_line(&mut metadata)?;
    let metadata: CrashtrackerMetadata = serde_json::from_str(&metadata)?;

    match receive_report(std::io::stdin().lock(), &metadata)? {
        CrashReportStatus::NoCrash => Ok(()),
        CrashReportStatus::CrashReport(crash_info) => {
            if config.resolve_frames == CrashtrackerResolveFrames::ExperimentalInReceiver {
                todo!("Processing names in the receiver is WIP");
            }
            if let Some(endpoint) = config.endpoint {
                // Don't keep the endpoint waiting forever.
                // TODO Experiment to see if 30 is the right number.
                crash_info.upload_to_endpoint(endpoint, Duration::from_secs(30))?;
            }
            Ok(())
        }
        CrashReportStatus::PartialCrashReport(_, _) => todo!(),
    }
}

/// The crashtracker collector sends data in blocks.
/// This enum tracks which block we're currently in, and, for multi-line blocks,
/// collects the partial data until the block is closed and it can be appended
/// to the CrashReport.
#[derive(Debug)]
enum StdinState {
    Counters,
    Done,
    File(String, Vec<String>),
    SigInfo,
    StackTrace(Vec<StackFrame>),
    Waiting,
}

/// A state machine that processes data from the crash-tracker collector line by
/// line.  The crashtracker collector sends data in blocks, so we use a `state`
/// variable to track which block we're in and collect partial data.
/// Once we reach the end of a block, append the block's data to `crashinfo`.
fn process_line(
    crashinfo: &mut CrashInfo,
    line: String,
    state: StdinState,
) -> anyhow::Result<StdinState> {
    let next = match state {
        StdinState::Counters if line.starts_with(DD_CRASHTRACK_END_COUNTERS) => StdinState::Waiting,
        StdinState::Counters => {
            let v: serde_json::Value = serde_json::from_str(&line)?;
            let map = v.as_object().context("Expected map type value")?;
            anyhow::ensure!(map.len() == 1);
            let (key, val) = map
                .iter()
                .next()
                .context("we know there is one value here")?;
            let val = val.as_i64().context("Vals are ints")?;
            crashinfo.add_counter(key, val)?;
            StdinState::Counters
        }

        StdinState::Done => {
            eprint!("Unexpected line after crashreport is done: {line}");
            StdinState::Done
        }

        StdinState::File(filename, lines) if line.starts_with(DD_CRASHTRACK_END_FILE) => {
            crashinfo.add_file_with_contents(&filename, lines)?;
            StdinState::Waiting
        }
        StdinState::File(name, mut contents) => {
            contents.push(line);
            StdinState::File(name, contents)
        }

        StdinState::SigInfo if line.starts_with(DD_CRASHTRACK_END_SIGINFO) => StdinState::Waiting,
        StdinState::SigInfo => {
            let siginfo = serde_json::from_str(&line)?;
            crashinfo.set_siginfo(siginfo)?;
            crashinfo.set_timestamp_to_now()?;
            StdinState::SigInfo
        }

        StdinState::StackTrace(stacktrace) if line.starts_with(DD_CRASHTRACK_END_STACKTRACE) => {
            crashinfo.set_stacktrace(stacktrace)?;
            StdinState::Waiting
        }
        StdinState::StackTrace(mut stacktrace) => {
            let frame = serde_json::from_str(&line).context(line)?;
            stacktrace.push(frame);
            StdinState::StackTrace(stacktrace)
        }

        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_COUNTERS) => {
            StdinState::Counters
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_FILE) => {
            let (_, filename) = line.split_once(' ').unwrap_or(("", "MISSING_FILENAME"));
            StdinState::File(filename.to_string(), vec![])
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_SIGINFO) => StdinState::SigInfo,
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_STACKTRACE) => {
            StdinState::StackTrace(vec![])
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_DONE) => StdinState::Done,
        StdinState::Waiting => {
            //TODO: Do something here?
            eprint!("Unexpected line while receiving crashreport: {line}");
            StdinState::Waiting
        }
    };
    Ok(next)
}

enum CrashReportStatus {
    NoCrash,
    CrashReport(CrashInfo),
    PartialCrashReport(CrashInfo, StdinState),
}

/// Listens to `stdin`, reading it line by line, until
/// 1. A crash-report is received, in which case it is processed for upload
/// 2. `stdin` closes without a crash report (i.e. if the parent terminated
///    normally)
/// In the case where the parent failed to transfer a full crash-report
/// (for instance if it crashed while calculating the crash-report), we return
/// a PartialCrashReport.
fn receive_report(
    stream: impl std::io::BufRead,
    metadata: &CrashtrackerMetadata,
) -> anyhow::Result<CrashReportStatus> {
    let mut crashinfo = CrashInfo::new(metadata.clone());
    let mut stdin_state = StdinState::Waiting;
    //TODO: This assumes that the input is valid UTF-8.
    for line in stream.lines() {
        let line = line?;
        stdin_state = process_line(&mut crashinfo, line, stdin_state)?;
    }

    if !crashinfo.crash_seen() {
        return Ok(CrashReportStatus::NoCrash);
    }

    #[cfg(target_os = "linux")]
    crashinfo.add_file("/proc/meminfo")?;
    #[cfg(target_os = "linux")]
    crashinfo.add_file("/proc/cpuinfo")?;

    // If we were waiting for data when stdin closed, let our caller know that
    // we only have partial data.
    if matches!(stdin_state, StdinState::Done) {
        Ok(CrashReportStatus::CrashReport(crashinfo))
    } else {
        Ok(CrashReportStatus::PartialCrashReport(
            crashinfo,
            stdin_state,
        ))
    }
}
