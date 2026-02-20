// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{
    crash_info::{
        CrashInfo, CrashInfoBuilder, ErrorKind, SigInfo, Span, StackFrame, TelemetryCrashUploader,
    },
    runtime_callback::RuntimeStack,
    shared::constants::*,
    CrashtrackerConfiguration, StackTrace,
};

use anyhow::Context;
use libdd_telemetry::data::LogLevel;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncBufReadExt;

#[derive(Debug)]
enum ReceiverIssue {
    Timeout,
    IoError,
    ProcessLine,
    AttachAdditionalFile,
    IncompleteStacktrace,
    UnexpectedLine,
}

impl ReceiverIssue {
    fn tag(&self) -> &'static str {
        match self {
            ReceiverIssue::Timeout => "receiver_issue:timeout",
            ReceiverIssue::IoError => "receiver_issue:io_error",
            ReceiverIssue::ProcessLine => "receiver_issue:process_line_error",
            ReceiverIssue::AttachAdditionalFile => "receiver_issue:attach_additional_file_error",
            ReceiverIssue::IncompleteStacktrace => "receiver_issue:incomplete_stacktrace",
            ReceiverIssue::UnexpectedLine => "receiver_issue:unexpected_line",
        }
    }
}

fn emit_debug_log(
    logger: &Option<Arc<TelemetryCrashUploader>>,
    issue: ReceiverIssue,
    crash_uuid: &str,
    message: String,
    level: LogLevel,
) {
    if let Some(logger) = logger.as_ref().map(Arc::clone) {
        let tags = format!(
            "{},crash_uuid:{},is_crash_debug:true",
            issue.tag(),
            crash_uuid
        );
        tokio::spawn(async move {
            let _ = logger.upload_general_log(message, tags, level).await;
        });
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct RuntimeStackFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    column: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    function: Vec<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    type_name: Vec<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    file: Vec<u8>,
}

impl From<RuntimeStackFrame> for StackFrame {
    fn from(value: RuntimeStackFrame) -> Self {
        let mut stack_frame = StackFrame::new();
        stack_frame.function = if value.function.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(&value.function).to_string())
        };
        stack_frame.type_name = if value.type_name.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(&value.type_name).to_string())
        };
        stack_frame.file = if value.file.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(&value.file).to_string())
        };
        stack_frame.line = value.line;
        stack_frame.column = value.column;
        stack_frame
    }
}

/// The crashtracker collector sends data in blocks.
/// This enum tracks which block we're currently in, and, for multi-line blocks,
/// collects the partial data until the block is closed and it can be appended
/// to the CrashReport.
#[derive(Debug)]
pub(crate) enum StdinState {
    AdditionalTags,
    Config,
    Counters,
    Done,
    File(String, Vec<String>),
    Kind,
    Metadata,
    ProcInfo,
    SigInfo,
    SpanIds,
    StackTrace,
    TraceIds,
    Ucontext,
    Waiting,
    WholeStackTrace,
    ThreadName(Option<String>),
    // StackFrame is always emitted as one stream of all the frames but StackString
    // may have lines that we need to accumulate depending on runtime (e.g. Python)
    RuntimeStackFrame(Vec<StackFrame>),
    RuntimeStackString(Vec<String>),
    Message,
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
    telemetry_logger: &Option<Arc<TelemetryCrashUploader>>,
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

        StdinState::WholeStackTrace if line.starts_with(DD_CRASHTRACK_END_WHOLE_STACKTRACE) => {
            StdinState::Waiting
        }
        StdinState::WholeStackTrace => {
            let stacktrace: StackTrace = serde_json::from_str(line)?;
            builder.with_stack(stacktrace)?;
            StdinState::WholeStackTrace
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

        StdinState::Kind if line.starts_with(DD_CRASHTRACK_END_KIND) => StdinState::Waiting,
        StdinState::Kind => {
            let kind: ErrorKind = serde_json::from_str(line)?;
            builder.with_kind(kind)?;
            StdinState::Kind
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
        StdinState::RuntimeStackFrame(frames)
            if line.starts_with(DD_CRASHTRACK_END_RUNTIME_STACK_FRAME) =>
        {
            let runtime_stack = RuntimeStack {
                format: "Datadog Runtime Callback 1.0".to_string(),
                frames,
                stacktrace_string: None,
            };
            builder.with_experimental_runtime_stack(runtime_stack)?;
            StdinState::Waiting
        }
        StdinState::RuntimeStackFrame(mut frames) => {
            let frame_json: RuntimeStackFrame = serde_json::from_str(line)?;
            frames.push(frame_json.into());
            StdinState::RuntimeStackFrame(frames)
        }
        StdinState::RuntimeStackString(lines)
            if line.starts_with(DD_CRASHTRACK_END_RUNTIME_STACK_STRING) =>
        {
            let runtime_stack = RuntimeStack {
                format: "Datadog Runtime Callback 1.0".to_string(),
                frames: vec![],
                stacktrace_string: Some(lines.join("\n")),
            };
            builder.with_experimental_runtime_stack(runtime_stack)?;
            StdinState::Waiting
        }
        StdinState::RuntimeStackString(mut lines) => {
            lines.push(line.to_string());
            StdinState::RuntimeStackString(lines)
        }
        StdinState::SigInfo if line.starts_with(DD_CRASHTRACK_END_SIGINFO) => StdinState::Waiting,
        StdinState::SigInfo => {
            let sig_info: SigInfo = serde_json::from_str(line)?;
            if !builder.has_message() {
                let message = format!(
                    "Process terminated with {:?} ({:?})",
                    sig_info.si_code_human_readable, sig_info.si_signo_human_readable
                );
                builder.with_message(message)?;
            }

            builder.with_timestamp_now()?;
            builder.with_sig_info(sig_info)?;
            builder.with_incomplete(true)?;
            StdinState::SigInfo
        }

        StdinState::Message if line.starts_with(DD_CRASHTRACK_END_MESSAGE) => StdinState::Waiting,
        StdinState::Message => {
            builder.with_message(line.to_string())?;
            StdinState::Message
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

        StdinState::ThreadName(thread_name) if line.starts_with(DD_CRASHTRACK_END_THREAD_NAME) => {
            if let Some(thread_name) = thread_name {
                builder.with_thread_name(thread_name)?;
            } else {
                builder.with_log_message(
                    "Thread name block ended without content".to_string(),
                    true,
                )?;
            }
            StdinState::Waiting
        }
        StdinState::ThreadName(_) => {
            let name = line.trim_end_matches('\n').to_string();
            StdinState::ThreadName(Some(name))
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
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_KIND) => StdinState::Kind,
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_METADATA) => {
            StdinState::Metadata
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_PROCINFO) => {
            StdinState::ProcInfo
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_SIGINFO) => StdinState::SigInfo,
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_MESSAGE) => StdinState::Message,
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_SPAN_IDS) => {
            StdinState::SpanIds
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_STACKTRACE) => {
            StdinState::StackTrace
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_RUNTIME_STACK_STRING) => {
            StdinState::RuntimeStackString(vec![])
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_RUNTIME_STACK_FRAME) => {
            StdinState::RuntimeStackFrame(vec![])
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_TRACE_IDS) => {
            StdinState::TraceIds
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_THREAD_NAME) => {
            StdinState::ThreadName(None)
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_UCONTEXT) => {
            StdinState::Ucontext
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_WHOLE_STACKTRACE) => {
            StdinState::WholeStackTrace
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_DONE) => {
            builder.with_incomplete(false)?;
            StdinState::Done
        }
        StdinState::Waiting => {
            let msg = format!("Unexpected line while receiving crashreport: {line}");
            builder.with_log_message(msg.clone(), true)?;
            emit_debug_log(
                telemetry_logger,
                ReceiverIssue::UnexpectedLine,
                &builder.uuid.to_string(),
                msg,
                LogLevel::Warn,
            );
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
    let mut telemetry_logger: Option<Arc<TelemetryCrashUploader>> = None;

    let mut crash_ping_sent = false;

    let mut lines = stream.lines();
    let mut deadline = None;
    // Start the timeout counter when the deadline when the first crash message is recieved
    let mut remaining_timeout = Duration::MAX;

    //TODO: This assumes that the input is valid UTF-8.
    loop {
        // Initialize telemetry logger once we have both config and metadata.
        if telemetry_logger.is_none() {
            if let (Some(cfg), Some(md)) = (&config, builder.metadata.clone()) {
                if let Ok(logger) = TelemetryCrashUploader::new(&md, cfg.endpoint()) {
                    telemetry_logger = Some(Arc::new(logger));
                }
            }
        }

        // We need to wait until at least we receive config, metadata, and kind (on non-Windows
        // platforms) before sending the crash ping
        if !crash_ping_sent && builder.is_ping_ready() {
            if let Some(ref config_ref) = config {
                let config_clone = config_ref.clone();
                crash_ping_sent = true;
                // Spawn crash ping sending in a separate task
                let crash_ping = builder.build_crash_ping()?;

                tokio::task::spawn(async move {
                    if let Err(e) = crash_ping
                        .upload_to_endpoint_async(config_clone.endpoint())
                        .await
                    {
                        eprintln!("Failed to send crash ping: {e}");
                    }
                });
            } else {
                eprintln!("No config found, skipping crash ping");
            }
        }
        let next_line = tokio::time::timeout(remaining_timeout, lines.next_line()).await;
        let Ok(next_line) = next_line else {
            builder.with_log_message(format!("Timeout: {next_line:?}"), true)?;
            emit_debug_log(
                &telemetry_logger,
                ReceiverIssue::Timeout,
                &builder.uuid.to_string(),
                format!("Timeout while waiting for crash report input: {next_line:?}"),
                LogLevel::Warn,
            );
            break;
        };
        let Ok(next_line) = next_line else {
            builder.with_log_message(format!("IO Error: {next_line:?}"), true)?;
            // We ignore error from uploading the log to telemetry, because what are we going to do?
            // If upload is failing, its not worth the effort to retry the request so we should just
            // continue on. At least we will get the log message in the crash info
            emit_debug_log(
                &telemetry_logger,
                ReceiverIssue::IoError,
                &builder.uuid.to_string(),
                format!("IO error while reading crash report input: {next_line:?}"),
                LogLevel::Warn,
            );
            break;
        };
        let Some(next_line) = next_line else { break };

        match process_line(
            &mut builder,
            &mut config,
            &next_line,
            stdin_state,
            &telemetry_logger,
        ) {
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
                emit_debug_log(
                    &telemetry_logger,
                    ReceiverIssue::ProcessLine,
                    &builder.uuid.to_string(),
                    format!("Unable to process line: {next_line}. Error: {e}"),
                    LogLevel::Warn,
                );
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

    enrich_thread_name(&mut builder)?;
    builder.with_os_info_this_machine()?;

    // Without a config, we don't even know the endpoint to transmit to.  Not much to do to recover.
    let config = config.context("Missing crashtracker configuration")?;

    for filename in config.additional_files() {
        if let Err(e) = builder.with_file(filename.clone()) {
            builder.with_log_message(e.to_string(), true)?;
            emit_debug_log(
                &telemetry_logger,
                ReceiverIssue::AttachAdditionalFile,
                &builder.uuid.to_string(),
                format!("Unable to attach additional file {filename:?}: {e}"),
                LogLevel::Warn,
            );
        }
    }

    let crash_info = builder.build()?;

    if crash_info.incomplete {
        emit_debug_log(
            &telemetry_logger,
            ReceiverIssue::IncompleteStacktrace,
            &crash_info.uuid,
            "CrashInfo stacktrace incomplete".to_string(),
            LogLevel::Warn,
        );
    }

    Ok(Some((config, crash_info)))
}

#[cfg(target_os = "linux")]
fn enrich_thread_name(builder: &mut CrashInfoBuilder) -> anyhow::Result<()> {
    use std::{fs, path::PathBuf};

    if builder.error.thread_name.is_some() {
        return Ok(());
    }
    let Some(proc_info) = builder.proc_info.as_ref() else {
        return Ok(());
    };
    let Some(tid) = proc_info.tid else {
        return Ok(());
    };
    let pid = proc_info.pid;
    let path = PathBuf::from(format!("/proc/{pid}/task/{tid}/comm"));
    let Ok(comm) = fs::read_to_string(&path) else {
        return Ok(());
    };
    let thread_name = comm.trim_end_matches('\n');
    if thread_name.is_empty() {
        return Ok(());
    }
    builder.with_thread_name(thread_name.to_string())?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn enrich_thread_name(_builder: &mut CrashInfoBuilder) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stdin_state_waiting_to_message() {
        let mut builder = CrashInfoBuilder::new();
        let mut config = None;

        let state = StdinState::Waiting;
        let line = DD_CRASHTRACK_BEGIN_MESSAGE;

        let next_state = process_line(&mut builder, &mut config, line, state, &None).unwrap();

        assert!(matches!(next_state, StdinState::Message));
    }

    #[test]
    fn test_stdin_state_message_content() {
        let mut builder = CrashInfoBuilder::new();
        let mut config = None;

        // Enter message state
        let state = StdinState::Message;
        let message_line = "program panicked";

        let next_state =
            process_line(&mut builder, &mut config, message_line, state, &None).unwrap();

        // Should stay in message state
        assert!(matches!(next_state, StdinState::Message));

        // Verify message was stored
        assert!(builder.has_message());
    }

    #[test]
    fn test_stdin_state_message_to_waiting() {
        let mut builder = CrashInfoBuilder::new();
        let mut config = None;

        let state = StdinState::Message;
        let line = DD_CRASHTRACK_END_MESSAGE;

        let next_state = process_line(&mut builder, &mut config, line, state, &None).unwrap();

        assert!(matches!(next_state, StdinState::Waiting));
    }

    #[test]
    fn test_message_state_with_empty_line() {
        let mut builder = CrashInfoBuilder::new();
        let mut config = None;

        let state = StdinState::Message;
        let empty_line = "";

        let result = process_line(&mut builder, &mut config, empty_line, state, &None);

        // Should handle empty line without error
        assert!(result.is_ok());
    }

    #[test]
    fn test_message_state_with_multiline_content() {
        let mut builder = CrashInfoBuilder::new();
        let mut config = None;

        // First line of message
        let state = process_line(
            &mut builder,
            &mut config,
            "Line 1 of panic",
            StdinState::Message,
            &None,
        )
        .unwrap();

        // Should still be in message state
        assert!(matches!(state, StdinState::Message));

        // Note: Current implementation may only store last message
        // This test documents current behavior
    }

    #[test]
    fn test_message_state_full_workflow() {
        let mut builder = CrashInfoBuilder::new();
        let mut config = None;

        // Start in waiting state
        let mut state = StdinState::Waiting;

        // Transition to message
        state = process_line(
            &mut builder,
            &mut config,
            DD_CRASHTRACK_BEGIN_MESSAGE,
            state,
            &None,
        )
        .unwrap();
        assert!(matches!(state, StdinState::Message));

        // Add message content
        state = process_line(
            &mut builder,
            &mut config,
            "test panic message",
            state,
            &None,
        )
        .unwrap();
        assert!(matches!(state, StdinState::Message));
        assert!(builder.has_message());

        // End message
        state = process_line(
            &mut builder,
            &mut config,
            DD_CRASHTRACK_END_MESSAGE,
            state,
            &None,
        )
        .unwrap();
        assert!(matches!(state, StdinState::Waiting));
    }

    #[test]
    fn test_stacktrace_empty_workflow() {
        // Test that receiving BEGIN_STACKTRACE followed by END_STACKTRACE
        // (with no frames) creates an empty but complete stack
        let mut builder = CrashInfoBuilder::new();
        let mut config = None;

        let mut state = StdinState::Waiting;

        state = process_line(
            &mut builder,
            &mut config,
            DD_CRASHTRACK_BEGIN_STACKTRACE,
            state,
            &None,
        )
        .unwrap();
        assert!(matches!(state, StdinState::StackTrace));

        // End stacktrace immediately (no frames)
        state = process_line(
            &mut builder,
            &mut config,
            DD_CRASHTRACK_END_STACKTRACE,
            state,
            &None,
        )
        .unwrap();
        assert!(matches!(state, StdinState::Waiting));

        // Verify we have an empty but incomplete stack (no frames captured = stack unwinding
        // failed)
        let stack = builder.error.stack.as_ref().expect("Stack should exist");
        assert!(stack.frames.is_empty());
        assert!(
            stack.incomplete,
            "Stack should be marked incomplete when no frames were captured"
        );

        // Verify a log message was recorded about no frames
        assert!(builder
            .log_messages
            .as_ref()
            .map(|msgs| msgs
                .iter()
                .any(|msg| msg.contains("No native stack frames received")))
            .unwrap_or(false));
    }

    #[test]
    fn test_stacktrace_with_frames_workflow() {
        let mut builder = CrashInfoBuilder::new();
        let mut config = None;

        let mut state = StdinState::Waiting;

        // Begin stacktrace
        state = process_line(
            &mut builder,
            &mut config,
            DD_CRASHTRACK_BEGIN_STACKTRACE,
            state,
            &None,
        )
        .unwrap();
        assert!(matches!(state, StdinState::StackTrace));

        // Add a frame
        let frame_json = r#"{"ip":"0x1234"}"#;
        state = process_line(&mut builder, &mut config, frame_json, state, &None).unwrap();
        assert!(matches!(state, StdinState::StackTrace));

        // End stacktrace
        state = process_line(
            &mut builder,
            &mut config,
            DD_CRASHTRACK_END_STACKTRACE,
            state,
            &None,
        )
        .unwrap();
        assert!(matches!(state, StdinState::Waiting));

        // Verify we have a stack with one frame, marked complete
        let stack = builder.error.stack.as_ref().expect("Stack should exist");
        assert_eq!(stack.frames.len(), 1);
        assert!(!stack.incomplete, "Stack should be marked complete");
        assert_eq!(stack.frames[0].ip, Some("0x1234".to_string()));
    }
}
