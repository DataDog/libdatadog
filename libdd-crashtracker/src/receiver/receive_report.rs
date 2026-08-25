// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{
    crash_info::{CrashInfo, CrashInfoBuilder, ErrorKind, SigInfo, Span, StackFrame, Ucontext},
    receiver::debug_logger::{DebugLogger, ReceiverIssue},
    runtime_callback::RuntimeStack,
    shared::constants::*,
    CrashtrackerConfiguration, StackTrace,
};

use anyhow::Context;
use libdd_telemetry::data::LogLevel;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::io::AsyncBufReadExt;

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
    debug_logger: &DebugLogger,
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
            let unescaped = line.replace("\\n", "\n").replace("\\r", "\r");
            builder.with_message(unescaped)?;
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
            let ucontext: Ucontext = serde_json::from_str(line)?;
            builder.with_ucontext(ucontext)?;
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
            debug_logger.emit(
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
///
/// Borrows `stream` rather than consuming it. The crashing process blocks on
/// POLLHUP from this connection, so closing it here would release that process
/// before the caller has symbolized the report, and symbolization reads
/// `/proc/<pid>/maps`. The caller decides when to close.
pub(crate) async fn receive_report_from_stream(
    timeout: Duration,
    stream: &mut (impl AsyncBufReadExt + std::marker::Unpin),
) -> anyhow::Result<Option<(CrashtrackerConfiguration, CrashInfo)>> {
    let mut builder = CrashInfoBuilder::new();
    let mut stdin_state = StdinState::Waiting;
    let mut config: Option<CrashtrackerConfiguration> = None;
    // Usable before the collector sends anything, so a receiver that never gets
    // a config or metadata block can still report why. Upgraded in the loop as
    // those blocks arrive.
    let mut debug_logger = DebugLogger::new(None, None);

    let mut crash_ping_sent = false;

    let mut lines = stream.lines();
    let mut deadline = None;
    // Start the timeout counter when the deadline when the first crash message is recieved
    let mut remaining_timeout = Duration::MAX;

    //TODO: This assumes that the input is valid UTF-8.
    loop {
        // Re-point the debug logger at the real endpoint and application once
        // the config and metadata blocks arrive. Cheap no-op until they do.
        debug_logger.update(config.as_ref(), builder.metadata.as_ref());

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
            debug_logger.emit(
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
            debug_logger.emit(
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
            &debug_logger,
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
                debug_logger.emit(
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
        // Nothing arrived at all, so there is no crash report to build and no
        // config to upload it with. The env-derived logger is all we have, and
        // this log is the only signal that the receiver ran and got nothing.
        // Waited on rather than spawned: we return right after, and the caller
        // drops the runtime, which would cancel a pending spawned task.
        debug_logger
            .emit_and_wait(
                ReceiverIssue::NoData,
                &builder.uuid.to_string(),
                "Receiver received no data".to_string(),
                LogLevel::Warn,
            )
            .await;
        return Ok(None);
    }

    enrich_thread_name(&mut builder)?;
    builder.with_os_info_this_machine()?;

    // Without a config, we don't even know the endpoint to transmit to.  Not much to do to recover.
    let config = config.context("Missing crashtracker configuration")?;

    for filename in config.additional_files() {
        if let Err(e) = builder.with_file(filename.clone()) {
            builder.with_log_message(e.to_string(), true)?;
            debug_logger.emit(
                ReceiverIssue::AttachAdditionalFile,
                &builder.uuid.to_string(),
                format!("Unable to attach additional file {filename:?}: {e}"),
                LogLevel::Warn,
            );
        }
    }

    // Thread collection is budgeted against the *remaining* receiver timeout;
    // whatever time is left after reading the crash data from stdin.
    // This makes sure the total receiver lifetime is bounded by the configured
    // timeout, and we always emit whatever threads were collected before
    // the deadline rather than silently discarding them.
    #[cfg(target_os = "linux")]
    if config.collect_all_threads() {
        if let Some(proc_info) = builder.proc_info.as_ref() {
            let parent_pid = proc_info.pid;
            let crashing_tid = proc_info.tid;
            // If we never received a first line (deadline is None) use zero so
            // collection is skipped; there is nothing to attach to anyway.
            let remaining_budget = deadline
                .map(|d| d.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::ZERO);
            if let Err(e) = collect_and_add_thread_contexts(
                &mut builder,
                &config,
                parent_pid,
                crashing_tid,
                remaining_budget,
            ) {
                let _ = builder
                    .with_log_message(format!("Failed to collect thread contexts: {e}"), true);
            }
        }
    }

    let crash_info = builder.build()?;

    if crash_info.incomplete {
        debug_logger.emit(
            ReceiverIssue::IncompleteStacktrace,
            &crash_info.uuid,
            "CrashInfo stacktrace incomplete".to_string(),
            LogLevel::Warn,
        );
    }

    Ok(Some((config, crash_info)))
}

#[cfg(target_os = "linux")]
fn collect_and_add_thread_contexts(
    builder: &mut CrashInfoBuilder,
    config: &CrashtrackerConfiguration,
    parent_pid: u32,
    crashing_tid: Option<u32>,
    budget: Duration,
) -> anyhow::Result<()> {
    use crate::crash_info::{StackTrace, ThreadData};
    use crate::receiver::ptrace_collector::stream_thread_contexts;

    let crashing_tid = crashing_tid.unwrap_or(0) as i32;
    let parent_pid = parent_pid as i32;

    let mut collected_threads = Vec::new();
    let crashing_context = builder.ucontext.clone();
    let mut crashing_stack = None;

    let incomplete = stream_thread_contexts(
        parent_pid,
        crashing_tid,
        config.max_threads(),
        budget,
        crashing_context.as_ref(),
        |tid, captured_context| {
            let (name, state) = read_thread_stat(parent_pid, tid);
            let name = name.unwrap_or_else(|| tid.to_string());

            let stack = match captured_context {
                Some(ctx) => ctx.stack_trace.clone(),
                None => StackTrace::new_incomplete(),
            };

            if tid == crashing_tid && !stack.frames.is_empty() {
                crashing_stack = Some(stack.clone());
            }

            collected_threads.push(ThreadData {
                crashed: tid == crashing_tid,
                name,
                stack,
                state,
            });
        },
    )?;

    if incomplete {
        let _ = builder.with_counter("threads_incomplete".to_string(), 1);
    }

    // The frame stream sent by the signal-safe collector is deliberately only
    // a fallback. Replace it with the CFI unwind rooted at the saved kernel
    // context so Error Tracking groups and displays the causal crash stack.
    if let Some(stack) = crashing_stack {
        builder.with_stack(stack)?;
    }

    let _ = builder.with_threads(collected_threads);

    Ok(())
}

/// Read thread name and state from a single `/proc/{pid}/task/{tid}/stat` file.
///
/// The stat file format is: `pid (comm) state ...`
/// `comm` (the thread name) is enclosed between the first `(` and the last `)`
/// The state character immediately follows the closing `)`.
#[cfg(target_os = "linux")]
fn read_thread_stat(pid: i32, tid: i32) -> (Option<String>, Option<String>) {
    let content = match std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/stat")) {
        Ok(c) => c,
        Err(_) => return (None, None),
    };

    let Some(name_start) = content.find('(') else {
        return (None, None);
    };
    let Some(name_end) = content.rfind(')') else {
        return (None, None);
    };

    let name = Some(content[name_start + 1..name_end].to_string());
    let state = content[name_end + 1..]
        .split_whitespace()
        .next()
        .map(|s| s.to_string());

    (name, state)
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

    /// Reads from `socket` until `marker` shows up, then answers 200 so the
    /// uploader's request completes instead of waiting out its timeout.
    async fn serve_one_request(listener: tokio::net::TcpListener, marker: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut request = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = socket.read(&mut chunk).await.expect("read");
            if n == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..n]);
            if String::from_utf8_lossy(&request).contains(marker) {
                break;
            }
        }
        let _ = socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
            .await;
        let _ = socket.flush().await;
        String::from_utf8_lossy(&request).to_string()
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_receive_report_no_data_sends_debug_log() {
        // Stand in for the agent, so the debug log has somewhere to land
        // without a config block telling the receiver where to send.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        std::env::set_var(
            "DD_TRACE_AGENT_URL",
            format!("http://{}", listener.local_addr().unwrap()),
        );
        let server = tokio::spawn(async move {
            tokio::time::timeout(
                Duration::from_secs(5),
                serve_one_request(listener, "no_data"),
            )
            .await
            .expect("no telemetry request received")
        });

        let (sender, receiver) = tokio::net::UnixStream::pair().unwrap();
        // Close without sending anything, as a parent that exited normally does.
        drop(sender);

        let mut stream = tokio::io::BufReader::new(receiver);
        let report = receive_report_from_stream(Duration::from_secs(1), &mut stream)
            .await
            .unwrap();
        assert!(report.is_none());

        let request = server.await.unwrap();
        assert!(
            request.contains("receiver_issue:no_data"),
            "no_data tag missing from telemetry request: {request}"
        );
        assert!(
            request.contains("Receiver received no data"),
            "no_data message missing from telemetry request: {request}"
        );
    }

    #[test]
    fn test_stdin_state_waiting_to_message() {
        let mut builder = CrashInfoBuilder::new();
        let mut config = None;

        let state = StdinState::Waiting;
        let line = DD_CRASHTRACK_BEGIN_MESSAGE;

        let next_state = process_line(
            &mut builder,
            &mut config,
            line,
            state,
            &DebugLogger::disabled(),
        )
        .unwrap();

        assert!(matches!(next_state, StdinState::Message));
    }

    #[test]
    fn test_stdin_state_message_content() {
        let mut builder = CrashInfoBuilder::new();
        let mut config = None;

        // Enter message state
        let state = StdinState::Message;
        let message_line = "program panicked";

        let next_state = process_line(
            &mut builder,
            &mut config,
            message_line,
            state,
            &DebugLogger::disabled(),
        )
        .unwrap();

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

        let next_state = process_line(
            &mut builder,
            &mut config,
            line,
            state,
            &DebugLogger::disabled(),
        )
        .unwrap();

        assert!(matches!(next_state, StdinState::Waiting));
    }

    #[test]
    fn test_message_state_with_empty_line() {
        let mut builder = CrashInfoBuilder::new();
        let mut config = None;

        let state = StdinState::Message;
        let empty_line = "";

        let result = process_line(
            &mut builder,
            &mut config,
            empty_line,
            state,
            &DebugLogger::disabled(),
        );

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
            &DebugLogger::disabled(),
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
            &DebugLogger::disabled(),
        )
        .unwrap();
        assert!(matches!(state, StdinState::Message));

        // Add message content
        state = process_line(
            &mut builder,
            &mut config,
            "test panic message",
            state,
            &DebugLogger::disabled(),
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
            &DebugLogger::disabled(),
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
            &DebugLogger::disabled(),
        )
        .unwrap();
        assert!(matches!(state, StdinState::StackTrace));

        // End stacktrace immediately (no frames)
        state = process_line(
            &mut builder,
            &mut config,
            DD_CRASHTRACK_END_STACKTRACE,
            state,
            &DebugLogger::disabled(),
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
            &DebugLogger::disabled(),
        )
        .unwrap();
        assert!(matches!(state, StdinState::StackTrace));

        // Add a frame
        let frame_json = r#"{"ip":"0x1234"}"#;
        state = process_line(
            &mut builder,
            &mut config,
            frame_json,
            state,
            &DebugLogger::disabled(),
        )
        .unwrap();
        assert!(matches!(state, StdinState::StackTrace));

        // End stacktrace
        state = process_line(
            &mut builder,
            &mut config,
            DD_CRASHTRACK_END_STACKTRACE,
            state,
            &DebugLogger::disabled(),
        )
        .unwrap();
        assert!(matches!(state, StdinState::Waiting));

        // Verify we have a stack with one frame, marked complete
        let stack = builder.error.stack.as_ref().expect("Stack should exist");
        assert_eq!(stack.frames.len(), 1);
        assert!(!stack.incomplete, "Stack should be marked complete");
        assert_eq!(stack.frames[0].ip, Some("0x1234".to_string()));
    }

    #[test]
    fn test_message_with_escaped_sentinel_does_not_inject() {
        // Simulates what emit_message produces after sanitize_message_for_wire:
        // the sentinel strings are on a single escaped line, not separate lines.
        let mut builder = CrashInfoBuilder::new();
        let mut config = None;
        let mut state = StdinState::Waiting;

        // Enter message state
        state = process_line(
            &mut builder,
            &mut config,
            DD_CRASHTRACK_BEGIN_MESSAGE,
            state,
            &DebugLogger::disabled(),
        )
        .unwrap();
        assert!(matches!(state, StdinState::Message));

        // Feed the sanitized content (newlines escaped, so it's one line)
        let sanitized_line = format!(
            "Exception 'Evil'\\n{}\\n{}\\n{{}}\\n{}",
            DD_CRASHTRACK_END_MESSAGE, DD_CRASHTRACK_BEGIN_CONFIG, DD_CRASHTRACK_END_CONFIG,
        );
        state = process_line(
            &mut builder,
            &mut config,
            &sanitized_line,
            state,
            &DebugLogger::disabled(),
        )
        .unwrap();
        // Must still be in Message state. the escaped sentinels are just text
        assert!(
            matches!(state, StdinState::Message),
            "escaped sentinels must not trigger state transitions"
        );

        // Now the real end sentinel
        state = process_line(
            &mut builder,
            &mut config,
            DD_CRASHTRACK_END_MESSAGE,
            state,
            &DebugLogger::disabled(),
        )
        .unwrap();
        assert!(matches!(state, StdinState::Waiting));

        // No config should have been injected
        assert!(
            config.is_none(),
            "no config section should have been parsed"
        );
        assert!(builder.has_message());
    }
}
