// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]

use self::stacktrace::StackFrame;
use super::*;
use anyhow::Context;
use nix::unistd::getppid;
use std::{io::BufReader, os::unix::net::UnixListener, time::Duration};

pub fn resolve_frames(
    config: &CrashtrackerConfiguration,
    crash_info: &mut CrashInfo,
) -> anyhow::Result<()> {
    if config.resolve_frames == CrashtrackerResolveFrames::InReceiver {
        // The receiver is the direct child of the crashing process
        // TODO: This pid should be sent over the wire, so that
        // it can be used in a sidecar.
        let ppid: u32 = getppid().as_raw().try_into()?;
        crash_info.resolve_names_from_process(ppid)?
    }
    Ok(())
}

pub fn get_unix_socket(socket_path: &str) -> anyhow::Result<UnixListener> {
    if std::fs::metadata(socket_path).is_ok() {
        println!("A socket is already present. Deleting...");
        std::fs::remove_file(socket_path)
            .with_context(|| format!("could not delete previous socket at {:?}", socket_path))?;
    }

    let unix_listener =
        UnixListener::bind(socket_path).context("Could not create the unix socket")?;
    println!("bound to socket");
    Ok(unix_listener)
}

pub const SOCKET_PATH: &str = "/tmp/crash-report-socket";

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
    println!("start entry point");
    // TODO
    let listener = get_unix_socket(SOCKET_PATH)?;
    let (unix_stream, _) = listener.accept().expect("to accept a connection");
    let stream = BufReader::new(unix_stream);
    //    let stream = std::io::stdin().lock();
    println!("opened stream");
    match receive_report(stream)? {
        CrashReportStatus::NoCrash => Ok(()),
        CrashReportStatus::CrashReport(config, mut crash_info) => {
            resolve_frames(&config, &mut crash_info)?;

            if let Some(endpoint) = &config.endpoint {
                // TODO Experiment to see if 30 is the right number.
                crash_info.upload_to_endpoint(endpoint.clone(), Duration::from_secs(30))?;
            }
            crash_info.upload_to_telemetry(&config)?;

            Ok(())
        }
        CrashReportStatus::PartialCrashReport(config, mut crash_info, stdin_state) => {
            eprintln!("Failed to fully receive crash.  Exit state was: {stdin_state:?}");
            resolve_frames(&config, &mut crash_info)?;

            if let Some(endpoint) = &config.endpoint {
                // TODO Experiment to see if 30 is the right number.
                crash_info.upload_to_endpoint(endpoint.clone(), Duration::from_secs(30))?;
            }
            crash_info.upload_to_telemetry(&config)?;

            Ok(())
        }
    }
}

/// The crashtracker collector sends data in blocks.
/// This enum tracks which block we're currently in, and, for multi-line blocks,
/// collects the partial data until the block is closed and it can be appended
/// to the CrashReport.
#[derive(Debug)]
enum StdinState {
    Config,
    Counters,
    Done,
    File(String, Vec<String>),
    Metadata,
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
    config: &mut Option<CrashtrackerConfiguration>,
    line: String,
    state: StdinState,
) -> anyhow::Result<StdinState> {
    let next = match state {
        StdinState::Config if line.starts_with(DD_CRASHTRACK_END_CONFIG) => StdinState::Waiting,
        StdinState::Config => {
            if config.is_some() {
                // The config might contain sensitive data, don't log it.
                eprint!("Unexpected double config");
            }
            std::mem::swap(config, &mut Some(serde_json::from_str(&line)?));
            StdinState::Config
        }

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

        StdinState::Metadata if line.starts_with(DD_CRASHTRACK_END_METADATA) => StdinState::Waiting,
        StdinState::Metadata => {
            let metadata = serde_json::from_str(&line)?;
            crashinfo.set_metadata(metadata)?;
            StdinState::Metadata
        }

        StdinState::SigInfo if line.starts_with(DD_CRASHTRACK_END_SIGINFO) => StdinState::Waiting,
        StdinState::SigInfo => {
            let siginfo = serde_json::from_str(&line)?;
            crashinfo.set_siginfo(siginfo)?;
            crashinfo.set_timestamp_to_now()?;
            StdinState::SigInfo
        }

        StdinState::StackTrace(stacktrace) if line.starts_with(DD_CRASHTRACK_END_STACKTRACE) => {
            crashinfo.set_stacktrace(None, stacktrace)?;
            StdinState::Waiting
        }
        StdinState::StackTrace(mut stacktrace) => {
            let frame = serde_json::from_str(&line).context(line)?;
            stacktrace.push(frame);
            StdinState::StackTrace(stacktrace)
        }

        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_CONFIG) => StdinState::Config,
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_COUNTERS) => {
            StdinState::Counters
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_FILE) => {
            let (_, filename) = line.split_once(' ').unwrap_or(("", "MISSING_FILENAME"));
            StdinState::File(filename.to_string(), vec![])
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_METADATA) => {
            StdinState::Metadata
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
    CrashReport(CrashtrackerConfiguration, CrashInfo),
    PartialCrashReport(CrashtrackerConfiguration, CrashInfo, StdinState),
}

/// Listens to `stream`, reading it line by line, until
/// 1. A crash-report is received, in which case it is processed for upload
/// 2. `stdin` closes without a crash report (i.e. if the parent terminated normally)
/// In the case where the parent failed to transfer a full crash-report
/// (for instance if it crashed while calculating the crash-report), we return
/// a PartialCrashReport.
fn receive_report(stream: impl std::io::BufRead) -> anyhow::Result<CrashReportStatus> {
    let mut crashinfo = CrashInfo::new();
    let mut stdin_state = StdinState::Waiting;
    let mut config = None;

    //TODO: This assumes that the input is valid UTF-8.
    for line in stream.lines() {
        let line = line?;
        eprintln!("{line}");
        stdin_state = process_line(&mut crashinfo, &mut config, line, stdin_state)?;
    }

    if !crashinfo.crash_seen() {
        return Ok(CrashReportStatus::NoCrash);
    }

    #[cfg(target_os = "linux")]
    crashinfo.add_file("/proc/meminfo")?;
    #[cfg(target_os = "linux")]
    crashinfo.add_file("/proc/cpuinfo")?;

    let config = config.context("Missing crashtracker configuration")?;

    // If we were waiting for data when stdin closed, let our caller know that
    // we only have partial data.
    if matches!(stdin_state, StdinState::Done) {
        Ok(CrashReportStatus::CrashReport(config, crashinfo))
    } else {
        crashinfo.set_incomplete(true)?;
        Ok(CrashReportStatus::PartialCrashReport(
            config,
            crashinfo,
            stdin_state,
        ))
    }
}
