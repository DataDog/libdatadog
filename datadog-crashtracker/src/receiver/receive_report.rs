// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Unix socket stream parsing for crash tracker receiver.
//!
//! This module implements the receiver-side parsing of the Unix socket communication protocol.
//! It reads the structured crash data stream sent by the collector and reconstructs the
//! crash information and configuration objects.
//!
//! ## Stream Parsing Process
//!
//! The parser operates as a state machine that processes the delimited sections:
//!
//! 1. **Line-by-line reading**: Reads the stream with timeout protection
//! 2. **Delimiter matching**: Identifies section boundaries using protocol markers
//! 3. **Section accumulation**: Collects data between begin/end delimiters
//! 4. **JSON deserialization**: Converts section data into appropriate data structures
//! 5. **State transitions**: Moves between parsing states until completion marker
//!
//! ```text
//! ┌─────────────────┐    Read Line     ┌─────────────────┐    Match Delimiter
//! │ Unix Socket     │─────────────────►│ Line Buffer     │─────────────────────┐
//! │ Stream          │                  │                 │                     │
//! └─────────────────┘                  └─────────────────┘                     │
//!                                                                               │
//!                                                                               v
//! ┌─────────────────┐    Build Objects ┌─────────────────┐    Accumulate Data  │
//! │ CrashInfo +     │◄─────────────────│ Section Data    │◄────────────────────┘
//! │ Configuration   │                  │ Collection      │
//! └─────────────────┘                  └─────────────────┘
//! ```
//!
//! ## State Machine
//!
//! The [`StdinState`] enum tracks the current parsing state and accumulates data
//! for multi-line sections until complete.
//!
//! For complete protocol documentation, see [`crate::shared::unix_socket_communication`].

use crate::{
    crash_info::{CrashInfo, CrashInfoBuilder, ErrorKind, Span, TelemetryCrashUploader},
    shared::constants::*,
    CrashtrackerConfiguration,
};
use anyhow::Context;
use std::time::{Duration, Instant};
use tokio::io::AsyncBufReadExt;
use uuid::Uuid;

/// Sends a crash ping telemetry event to indicate that crash processing has started.
/// We no-op on file endpoints because unlike production environments, we know if
/// a crash report failed to send when file debugging.
async fn send_crash_ping_to_url(
    config: &CrashtrackerConfiguration,
    crash_uuid: &str,
    metadata: &crate::crash_info::Metadata,
    sig_info: &crate::crash_info::SigInfo,
) -> anyhow::Result<()> {
    let is_file_endpoint = config
        .endpoint()
        .as_ref()
        .map(|e| e.url.scheme_str() == Some("file"))
        .unwrap_or(false);

    if is_file_endpoint {
        return Ok(());
    }

    let uploader = TelemetryCrashUploader::new(metadata, config.endpoint())?;
    uploader.send_crash_ping(crash_uuid, sig_info).await?;
    Ok(())
}

/// State machine for parsing Unix socket crash data stream.
///
/// This enum tracks the current parsing state as the receiver processes the structured
/// crash data stream. Each variant represents a different section of the crash report
/// protocol, and for multi-line sections, accumulates partial data until the section
/// is complete.
///
/// ## State Transitions
///
/// The parser transitions between states based on delimiter markers:
/// - `DD_CRASHTRACK_BEGIN_*` markers transition to data collection states
/// - `DD_CRASHTRACK_END_*` markers complete sections and process accumulated data
/// - `DD_CRASHTRACK_DONE` transitions to the final Done state
///
/// ## Multi-line Section Handling
///
/// Some states like `File` and `Stacktrace` accumulate multiple lines of data
/// between their begin/end delimiters before processing.
#[derive(Debug)]
pub(crate) enum StdinState {
    /// Parsing additional tags section
    AdditionalTags,
    /// Parsing configuration section (JSON)
    Config,
    /// Parsing internal counters section
    Counters,
    /// Parsing complete - crash report transmission finished
    Done,
    /// Parsing file section (filename, content lines)
    File(String, Vec<String>),
    /// Parsing metadata section (JSON)
    Metadata,
    /// Parsing process information section (JSON)
    ProcInfo,
    SigInfo,
    SpanIds,
    StackTrace,
    TraceIds,
    Ucontext,
    Waiting,
}

/// A state machine that processes data from the crash-tracker collector line by
/// line.  The crashtracker collector sends data in blocks, so we use a `state`
/// variable to track which block we're in and collect partial data.
/// Once we reach the end of a block, append the block's data to `crashinfo`.
fn process_line(
    builder: &mut CrashInfoBuilder,
    config: &mut Option<CrashtrackerConfiguration>,
    line: &str,
    state: StdinState,
) -> anyhow::Result<StdinState> {
    let next = match state {
        StdinState::AdditionalTags if line.starts_with(DD_CRASHTRACK_END_ADDITIONAL_TAGS) => {
            StdinState::Waiting
        }
        StdinState::AdditionalTags => {
            let additional_tags: Vec<String> = serde_json::from_str(line)?;
            builder.with_experimental_additional_tags(additional_tags)?;
            StdinState::AdditionalTags
        }

        StdinState::Config if line.starts_with(DD_CRASHTRACK_END_CONFIG) => StdinState::Waiting,
        StdinState::Config => {
            if config.is_some() {
                // The config might contain sensitive data, don't log it.
                eprintln!("Unexpected double config");
            }
            *config = Some(serde_json::from_str(line)?);
            StdinState::Config
        }

        StdinState::Counters if line.starts_with(DD_CRASHTRACK_END_COUNTERS) => StdinState::Waiting,
        StdinState::Counters => {
            let v: serde_json::Value = serde_json::from_str(line)?;
            let map = v.as_object().context("Expected map type value")?;
            anyhow::ensure!(map.len() == 1);
            let (key, val) = map
                .iter()
                .next()
                .context("we know there is one value here")?;
            let val = val.as_i64().context("Vals are ints")?;
            builder.with_counter(key.clone(), val)?;
            StdinState::Counters
        }

        StdinState::Done => {
            builder.with_log_message(
                format!("Unexpected line after crashreport is done: {line}"),
                true,
            )?;
            StdinState::Done
        }

        StdinState::File(filename, lines) if line.starts_with(DD_CRASHTRACK_END_FILE) => {
            builder.with_file_and_contents(filename, lines)?;
            StdinState::Waiting
        }
        StdinState::File(name, mut contents) => {
            contents.push(line.to_string());
            StdinState::File(name, contents)
        }

        StdinState::Metadata if line.starts_with(DD_CRASHTRACK_END_METADATA) => StdinState::Waiting,
        StdinState::Metadata => {
            let metadata = serde_json::from_str(line)?;
            builder.with_metadata(metadata)?;
            StdinState::Metadata
        }

        StdinState::ProcInfo if line.starts_with(DD_CRASHTRACK_END_PROCINFO) => StdinState::Waiting,
        StdinState::ProcInfo => {
            let proc_info = serde_json::from_str(line)?;
            builder.with_proc_info(proc_info)?;
            StdinState::ProcInfo
        }

        StdinState::SigInfo if line.starts_with(DD_CRASHTRACK_END_SIGINFO) => StdinState::Waiting,
        StdinState::SigInfo => {
            let sig_info: crate::SigInfo = serde_json::from_str(line)?;
            // By convention, siginfo is the first thing sent.
            let message = format!(
                "Process terminated with {:?} ({:?})",
                sig_info.si_code_human_readable, sig_info.si_signo_human_readable
            );

            builder
                .with_timestamp_now()?
                .with_sig_info(sig_info)?
                .with_incomplete(true)?
                .with_message(message)?;
            StdinState::SigInfo
        }

        StdinState::SpanIds if line.starts_with(DD_CRASHTRACK_END_SPAN_IDS) => StdinState::Waiting,
        StdinState::SpanIds => {
            let span_ids: Vec<Span> = serde_json::from_str(line)?;
            builder.with_span_ids(span_ids)?;
            StdinState::SpanIds
        }

        StdinState::StackTrace if line.starts_with(DD_CRASHTRACK_END_STACKTRACE) => {
            builder.with_stack_set_complete()?;
            StdinState::Waiting
        }
        StdinState::StackTrace => {
            let frame = serde_json::from_str(line)?;
            builder.with_stack_frame(frame, true)?;
            StdinState::StackTrace
        }

        StdinState::TraceIds if line.starts_with(DD_CRASHTRACK_END_TRACE_IDS) => {
            StdinState::Waiting
        }
        StdinState::TraceIds => {
            let trace_ids: Vec<Span> = serde_json::from_str(line)?;
            builder.with_trace_ids(trace_ids)?;
            StdinState::TraceIds
        }
        StdinState::Ucontext if line.starts_with(DD_CRASHTRACK_END_UCONTEXT) => StdinState::Waiting,
        StdinState::Ucontext => {
            builder.with_experimental_ucontext(line.to_string())?;
            StdinState::Ucontext
        }

        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_ADDITIONAL_TAGS) => {
            StdinState::AdditionalTags
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
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_PROCINFO) => {
            StdinState::ProcInfo
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_SIGINFO) => StdinState::SigInfo,
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_SPAN_IDS) => {
            StdinState::SpanIds
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_STACKTRACE) => {
            StdinState::StackTrace
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_TRACE_IDS) => {
            StdinState::TraceIds
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_UCONTEXT) => {
            StdinState::Ucontext
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_DONE) => {
            builder.with_incomplete(false)?;
            StdinState::Done
        }
        StdinState::Waiting => {
            builder.with_log_message(
                format!("Unexpected line while receiving crashreport: {line}"),
                true,
            )?;
            StdinState::Waiting
        }
    };
    Ok(next)
}

/// Listens to `stream`, reading it line by line, until
/// 1. A crash-report is received, in which case it is processed for upload, and we return
///    Some(CrashInfo)
/// 2. `stdin` closes without a crash report (i.e. if the parent terminated normally). In this case
///    we return "None".
pub(crate) async fn receive_report_from_stream(
    timeout: Duration,
    stream: impl AsyncBufReadExt + std::marker::Unpin,
) -> anyhow::Result<Option<(CrashtrackerConfiguration, CrashInfo)>> {
    let mut builder = CrashInfoBuilder::new();
    let mut stdin_state = StdinState::Waiting;
    let mut config: Option<CrashtrackerConfiguration> = None;

    // Generate UUID early so we can use it for both crash ping and crash report
    let crash_uuid = Uuid::new_v4().to_string();
    let mut crash_ping_sent = false;

    let mut lines = stream.lines();
    let mut deadline = None;
    // Start the timeout counter when the deadline when the first crash message is recieved
    let mut remaining_timeout = Duration::MAX;

    //TODO: This assumes that the input is valid UTF-8.
    loop {
        if !crash_ping_sent {
            if let (Some(config), Some(metadata), Some(sig_info)) = (
                config.as_ref(),
                builder.metadata.as_ref(),
                builder.sig_info.as_ref(),
            ) {
                crash_ping_sent = true;
                // Spawn crash ping sending in a separate task
                let config_clone = config.clone();
                let metadata_clone = metadata.clone();
                let crash_uuid_clone = crash_uuid.clone();
                let sig_info_clone = sig_info.clone();
                tokio::task::spawn(async move {
                    if let Err(e) = send_crash_ping_to_url(
                        &config_clone,
                        &crash_uuid_clone,
                        &metadata_clone,
                        &sig_info_clone,
                    )
                    .await
                    {
                        eprintln!("Failed to send crash ping: {e}");
                    }
                });
            }
        }
        let next_line = tokio::time::timeout(remaining_timeout, lines.next_line()).await;
        let Ok(next_line) = next_line else {
            builder.with_log_message(format!("Timeout: {next_line:?}"), true)?;
            break;
        };
        let Ok(next_line) = next_line else {
            builder.with_log_message(format!("IO Error: {next_line:?}"), true)?;
            break;
        };
        let Some(next_line) = next_line else { break };

        match process_line(&mut builder, &mut config, &next_line, stdin_state) {
            Ok(next_state) => {
                stdin_state = next_state;
                if matches!(stdin_state, StdinState::Done) {
                    break;
                }
            }
            Err(e) => {
                // If the input is corrupted, stop and salvage what we can
                builder.with_log_message(
                    format!("Unable to process line: {next_line}. Error: {e}"),
                    true,
                )?;
                break;
            }
        }

        if let Some(deadline) = deadline {
            // The clock was already ticking, update the remaining time
            remaining_timeout = deadline - Instant::now()
        } else {
            // We've recieved the first message from the collector, start the clock ticking.
            deadline = Some(Instant::now() + timeout);
            remaining_timeout = timeout;
        }
    }

    if !builder.has_data() {
        return Ok(None);
    }

    // For now, we only support Signal based crash detection in the receiver.
    builder.with_kind(ErrorKind::UnixSignal)?;

    // Set the pre-generated UUID so both crash ping and crash report use the same ID
    builder.with_uuid(crash_uuid)?;

    // Without a config, we don't even know the endpoint to transmit to.  Not much to do to recover.
    let config = config.context("Missing crashtracker configuration")?;
    for filename in config.additional_files() {
        if let Err(e) = builder.with_file(filename.clone()) {
            builder.with_log_message(e.to_string(), true)?;
        }
    }

    let crash_info = builder.build()?;

    Ok(Some((config, crash_info)))
}
