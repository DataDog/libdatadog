// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]

use super::*;
use crate::shared::constants::*;
use anyhow::Context;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixListener;

pub fn resolve_frames(
    config: &CrashtrackerConfiguration,
    crash_info: &mut CrashInfo,
) -> anyhow::Result<()> {
    if config.resolve_frames == StacktraceCollection::EnabledWithSymbolsInReceiver {
        let proc_info = crash_info
            .proc_info
            .as_ref()
            .context("Unable to resolve frames: No PID specified")?;
        crash_info.resolve_names_from_process(proc_info.pid)?;
    }
    Ok(())
}

pub fn get_unix_socket(socket_path: impl AsRef<str>) -> anyhow::Result<UnixListener> {
    fn path_bind(socket_path: impl AsRef<str>) -> anyhow::Result<UnixListener> {
        let socket_path = socket_path.as_ref();
        if std::fs::metadata(socket_path).is_ok() {
            std::fs::remove_file(socket_path).with_context(|| {
                format!("could not delete previous socket at {:?}", socket_path)
            })?;
        }
        Ok(UnixListener::bind(socket_path)?)
    }

    #[cfg(target_os = "linux")]
    let unix_listener = if socket_path.as_ref().starts_with(['.', '/']) {
        path_bind(socket_path)
    } else {
        use std::os::linux::net::SocketAddrExt;
        std::os::unix::net::SocketAddr::from_abstract_name(socket_path.as_ref())
            .and_then(|addr| {
                std::os::unix::net::UnixListener::bind_addr(&addr)
                    .and_then(|listener| {
                        listener.set_nonblocking(true)?;
                        Ok(listener)
                    })
                    .and_then(UnixListener::from_std)
            })
            .map_err(anyhow::Error::msg)
    };
    #[cfg(not(target_os = "linux"))]
    let unix_listener = path_bind(socket_path);
    unix_listener.context("Could not create the unix socket")
}

pub fn receiver_entry_point_unix_socket(socket_path: impl AsRef<str>) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async_receiver_entry_point_unix_socket(socket_path, true))?;
    Ok(())
    // Dropping the stream closes it, allowing the collector to exit if it was waiting.
}

pub fn receiver_timeout() -> Duration {
    // https://github.com/DataDog/libdatadog/issues/717
    if let Ok(s) = std::env::var("DD_CRASHTRACKER_RECEIVER_TIMEOUT_MS") {
        if let Ok(v) = s.parse() {
            return Duration::from_millis(v);
        }
    }
    // Default value
    Duration::from_millis(4000)
}

pub fn receiver_entry_point_stdin() -> anyhow::Result<()> {
    let stream = BufReader::new(tokio::io::stdin());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(receiver_entry_point(receiver_timeout(), stream))?;
    Ok(())
}

pub async fn async_receiver_entry_point_unix_socket(
    socket_path: impl AsRef<str>,
    one_shot: bool,
) -> anyhow::Result<()> {
    let listener = get_unix_socket(socket_path)?;
    loop {
        let (unix_stream, _) = listener.accept().await?;
        let stream = BufReader::new(unix_stream);
        let res = receiver_entry_point(receiver_timeout(), stream).await;

        if one_shot {
            return res;
        }
    }
}

/// Receives data from a crash collector via a stream, formats it into
/// `CrashInfo` json, and emits it to the endpoint/file defined in `config`.
///
/// At a high-level, this exists because doing anything in a
/// signal handler is dangerous, so we fork a sidecar to do the stuff we aren't
/// allowed to do in the handler.
///
/// See comments in [crashtracker/lib.rs] for a full architecture
/// description.
async fn receiver_entry_point(
    timeout: Duration,
    stream: impl AsyncBufReadExt + std::marker::Unpin,
) -> anyhow::Result<()> {
    match receive_report(timeout, stream).await? {
        CrashReportStatus::NoCrash => Ok(()),
        CrashReportStatus::CrashReport(config, mut crash_info) => {
            resolve_frames(&config, &mut crash_info)?;
            crash_info.async_upload_to_endpoint(&config.endpoint).await
        }
        CrashReportStatus::PartialCrashReport(config, mut crash_info, stdin_state) => {
            eprintln!("Failed to fully receive crash.  Exit state was: {stdin_state:?}");
            resolve_frames(&config, &mut crash_info)?;
            crash_info.async_upload_to_endpoint(&config.endpoint).await
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
    InternalError(String),
    Metadata,
    ProcInfo,
    SigInfo,
    SpanIds,
    StackTrace(Vec<StackFrame>),
    TraceIds,
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
                eprintln!("Unexpected double config");
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
            eprintln!("Unexpected line after crashreport is done: {line}");
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

        StdinState::InternalError(e) => anyhow::bail!("Can't continue after internal error {e}"),

        StdinState::Metadata if line.starts_with(DD_CRASHTRACK_END_METADATA) => StdinState::Waiting,
        StdinState::Metadata => {
            let metadata = serde_json::from_str(&line)?;
            crashinfo.set_metadata(metadata)?;
            StdinState::Metadata
        }

        StdinState::ProcInfo if line.starts_with(DD_CRASHTRACK_END_PROCINFO) => StdinState::Waiting,
        StdinState::ProcInfo => {
            let proc_info = serde_json::from_str(&line)?;
            crashinfo.set_procinfo(proc_info)?;
            StdinState::ProcInfo
        }

        StdinState::SigInfo if line.starts_with(DD_CRASHTRACK_END_SIGINFO) => StdinState::Waiting,
        StdinState::SigInfo => {
            let siginfo = serde_json::from_str(&line)?;
            crashinfo.set_siginfo(siginfo)?;
            crashinfo.set_timestamp_to_now()?;
            StdinState::SigInfo
        }

        StdinState::SpanIds if line.starts_with(DD_CRASHTRACK_END_SPAN_IDS) => StdinState::Waiting,
        StdinState::SpanIds => {
            let v: Vec<u128> = serde_json::from_str(&line)?;
            crashinfo.set_span_ids(v)?;
            StdinState::SpanIds
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

        StdinState::TraceIds if line.starts_with(DD_CRASHTRACK_END_TRACE_IDS) => {
            StdinState::Waiting
        }
        StdinState::TraceIds => {
            let v: Vec<u128> = serde_json::from_str(&line)?;
            crashinfo.set_trace_ids(v)?;
            StdinState::TraceIds
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
            StdinState::StackTrace(vec![])
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_BEGIN_TRACE_IDS) => {
            StdinState::TraceIds
        }
        StdinState::Waiting if line.starts_with(DD_CRASHTRACK_DONE) => StdinState::Done,
        StdinState::Waiting => {
            //TODO: Do something here?
            eprintln!("Unexpected line while receiving crashreport: {line}");
            StdinState::Waiting
        }
    };
    Ok(next)
}

#[derive(Debug)]
enum CrashReportStatus {
    NoCrash,
    CrashReport(CrashtrackerConfiguration, CrashInfo),
    PartialCrashReport(CrashtrackerConfiguration, CrashInfo, StdinState),
}

/// Listens to `stream`, reading it line by line, until
/// 1. A crash-report is received, in which case it is processed for upload
/// 2. `stdin` closes without a crash report (i.e. if the parent terminated normally)
///
/// In the case where the parent failed to transfer a full crash-report
/// (for instance if it crashed while calculating the crash-report), we return
/// a PartialCrashReport.
async fn receive_report(
    timeout: Duration,
    stream: impl AsyncBufReadExt + std::marker::Unpin,
) -> anyhow::Result<CrashReportStatus> {
    let mut crashinfo = CrashInfo::new();
    let mut stdin_state = StdinState::Waiting;
    let mut config = None;

    let mut lines = stream.lines();
    let mut deadline = None;
    // Start the timeout counter when the deadline when the first crash message is recieved
    let mut remaining_timeout = Duration::MAX;

    //TODO: This assumes that the input is valid UTF-8.
    loop {
        let next = tokio::time::timeout(remaining_timeout, lines.next_line()).await;
        if let Err(elapsed) = next {
            eprintln!("Timeout: {elapsed}");
            break;
        };
        let next = next.unwrap();
        if let Err(io_err) = next {
            eprintln!("IO Error: {io_err}");
            break;
        }
        let next = next.unwrap();
        if next.is_none() {
            break;
        }
        let line = next.unwrap();

        match process_line(&mut crashinfo, &mut config, line, stdin_state) {
            Ok(next_state) => {
                stdin_state = next_state;
                if matches!(stdin_state, StdinState::Done) {
                    break;
                }
            }
            Err(e) => {
                // If the input is corrupted, stop and salvage what we can
                stdin_state = StdinState::InternalError(e.to_string());
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

    if !crashinfo.crash_seen() {
        return Ok(CrashReportStatus::NoCrash);
    }

    // Without a config, we don't even know the endpoint to transmit to.  Not much to do to recover.
    let config = config.context("Missing crashtracker configuration")?;
    for filename in &config.additional_files {
        crashinfo
            .add_file(filename)
            .unwrap_or_else(|e| eprintln!("Unable to add file {filename}: {e}"));
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixStream;

    async fn to_socket(
        target: &mut tokio::net::UnixStream,
        msg: impl AsRef<str>,
    ) -> anyhow::Result<usize> {
        let msg = msg.as_ref();
        let n = target.write(format!("{msg}\n").as_bytes()).await?;
        target.flush().await?;
        Ok(n)
    }

    async fn send_report(delay: Duration, mut stream: UnixStream) -> anyhow::Result<()> {
        let sender = &mut stream;
        to_socket(sender, DD_CRASHTRACK_BEGIN_SIGINFO).await?;
        to_socket(
            sender,
            serde_json::to_string(&SigInfo {
                signame: Some("SIGSEGV".to_string()),
                signum: 11,
                faulting_address: None,
            })?,
        )
        .await?;
        to_socket(sender, DD_CRASHTRACK_END_SIGINFO).await?;

        to_socket(sender, DD_CRASHTRACK_BEGIN_CONFIG).await?;
        to_socket(
            sender,
            serde_json::to_string(&CrashtrackerConfiguration::new(
                vec![],
                false,
                false,
                None,
                StacktraceCollection::Disabled,
                3000,
                None,
            )?)?,
        )
        .await?;
        to_socket(sender, DD_CRASHTRACK_END_CONFIG).await?;
        tokio::time::sleep(delay).await;
        to_socket(sender, DD_CRASHTRACK_DONE).await?;
        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_receive_report_short_timeout() -> anyhow::Result<()> {
        let (sender, receiver) = tokio::net::UnixStream::pair()?;

        let join_handle1 = tokio::spawn(receive_report(
            Duration::from_secs(1),
            BufReader::new(receiver),
        ));
        let join_handle2 = tokio::spawn(send_report(Duration::from_secs(2), sender));

        let crash_report = join_handle1.await??;
        assert!(matches!(
            crash_report,
            CrashReportStatus::PartialCrashReport(_, _, _)
        ));
        let sender_error = join_handle2.await?.unwrap_err().to_string();
        assert_eq!(sender_error, "Broken pipe (os error 32)");
        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_receive_report_long_timeout() -> anyhow::Result<()> {
        let (sender, receiver) = tokio::net::UnixStream::pair()?;

        let join_handle1 = tokio::spawn(receive_report(
            Duration::from_secs(2),
            BufReader::new(receiver),
        ));
        let join_handle2 = tokio::spawn(send_report(Duration::from_secs(1), sender));

        let crash_report = join_handle1.await??;
        assert!(matches!(crash_report, CrashReportStatus::CrashReport(_, _)));
        join_handle2.await??;
        Ok(())
    }
}
