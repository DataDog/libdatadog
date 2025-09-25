// SPDX-License-Identifier: Apache-2.0
// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/

//! Crash tracker receiver entry points for Unix socket communication.
//!
//! This module provides the receiver-side implementation of the crash tracker's
//! Unix socket communication protocol. The receiver processes crash data sent
//! by the collector via Unix domain sockets.
//!
//! ## Receiver Architecture
//!
//! The receiver operates in multiple modes:
//!
//! ### 1. Fork+Execve Mode (Primary)
//! ```text
//! ┌─────────────────────┐    Unix Socket     ┌─────────────────────┐
//! │ Collector Process   │──────────────────►│ Receiver Process    │
//! │ (Write crash data)  │                   │ (Read via stdin)    │
//! └─────────────────────┘                   └─────────────────────┘
//! ```
//!
//! ### 2. Named Socket Mode (Long-lived receiver)
//! ```text
//! ┌─────────────────────┐    Named Socket    ┌─────────────────────┐
//! │ Collector Process   │──────────────────►│ Long-lived Receiver │
//! │ (Connect to socket) │                   │ (Listen on socket)  │
//! └─────────────────────┘                   └─────────────────────┘
//! ```
//!
//! ## Processing Pipeline
//!
//! The receiver performs these operations on crash data:
//!
//! 1. **Parse Stream**: Read structured crash data using [`receive_report_from_stream()`]
//! 2. **Symbol Resolution**: Resolve stack frame symbols if configured
//! 3. **Name Demangling**: Demangle C++/Rust symbol names if enabled
//! 4. **Upload/Output**: Send formatted crash report to configured endpoint
//!
//! For complete protocol documentation, see [`crate::shared::unix_socket_communication`].
//!
//! [`receive_report_from_stream()`]: crate::receiver::receive_report::receive_report_from_stream

use super::receive_report::receive_report_from_stream;
use crate::{crash_info::CrashInfo, CrashtrackerConfiguration, StacktraceCollection};
use anyhow::Context;
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::UnixListener,
};

/*-----------------------------------------
|                Public API               |
------------------------------------------*/

pub fn receiver_entry_point_stdin() -> anyhow::Result<()> {
    let stream = BufReader::new(tokio::io::stdin());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(receiver_entry_point(receiver_timeout(), stream))?;
    Ok(())
}

pub async fn async_receiver_entry_point_unix_listener(
    listener: &UnixListener,
) -> anyhow::Result<()> {
    let (unix_stream, _) = listener.accept().await?;
    let stream = BufReader::new(unix_stream);
    receiver_entry_point(receiver_timeout(), stream).await
}

pub async fn async_receiver_entry_point_unix_socket(
    socket_path: impl AsRef<str>,
    one_shot: bool,
) -> anyhow::Result<()> {
    let listener = get_receiver_unix_socket(socket_path)?;
    loop {
        let res = async_receiver_entry_point_unix_listener(&listener).await;
        // TODO, should we log failures somewhere?
        if one_shot {
            return res;
        }
    }
}

pub fn receiver_entry_point_unix_socket(socket_path: impl AsRef<str>) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async_receiver_entry_point_unix_socket(socket_path, true))?;
    Ok(())
    // Dropping the stream closes it, allowing the collector to exit if it was waiting.
}

pub fn get_receiver_unix_socket(socket_path: impl AsRef<str>) -> anyhow::Result<UnixListener> {
    fn path_bind(socket_path: impl AsRef<str>) -> anyhow::Result<UnixListener> {
        let socket_path = socket_path.as_ref();
        if std::fs::metadata(socket_path).is_ok() {
            std::fs::remove_file(socket_path)
                .with_context(|| format!("could not delete previous socket at {socket_path:?}"))?;
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

/// Core receiver entry point that processes crash data from Unix socket stream.
///
/// This is the main processing function that handles crash data received via Unix domain sockets.
/// It parses the structured crash data stream, performs symbol resolution and name demangling,
/// and uploads the formatted crash report to the configured endpoint.
///
/// ## Processing Pipeline
///
/// 1. **Stream Parsing**: Reads crash data using the structured Unix socket protocol
/// 2. **Symbol Resolution**: Resolves memory addresses to function names, file names, and line numbers
/// 3. **Name Demangling**: Converts mangled C++/Rust symbols to readable names
/// 4. **Error Accumulation**: Collects any processing errors in the crash report
/// 5. **Upload**: Transmits the formatted crash report to the backend endpoint
///
/// ## Protocol Flow
///
/// ```text
/// Unix Socket Stream → Parse Sections → Resolve Symbols → Demangle Names → Upload
///        │                   │              │               │            │
///        v                   v              v               v            v
/// ┌─────────────┐   ┌─────────────┐   ┌─────────────┐   ┌────────┐   ┌─────────┐
/// │ Delimited   │   │ CrashInfo + │   │ Enriched    │   │ Readable│   │ Backend │
/// │ Sections    │──►│ Config      │──►│ Stack Frames│──►│ Symbols │──►│ Upload  │
/// │ (Protocol)  │   │ Objects     │   │ (Addresses) │   │ (Names) │   │ (JSON)  │
/// └─────────────┘   └─────────────┘   └─────────────┘   └────────┘   └─────────┘
/// ```
///
/// ## Arguments
///
/// * `timeout` - Maximum time to wait for complete crash data stream
/// * `stream` - Async buffered stream containing crash data (usually Unix socket via stdin)
///
/// ## Returns
///
/// * `Ok(())` - Crash report processed and uploaded successfully
/// * `Err(anyhow::Error)` - Stream parsing, processing, or upload failed
///
/// ## Timeout Behavior
///
/// If the crash data stream is incomplete or corrupted, the function will timeout
/// after the specified duration to prevent hanging indefinitely. The timeout can
/// be configured via `DD_CRASHTRACKER_RECEIVER_TIMEOUT_MS` environment variable.
///
/// ## Error Handling
///
/// Processing errors (symbol resolution, demangling) are non-fatal and are accumulated
/// in the crash report's log messages. Only stream parsing and upload errors cause
/// the function to return an error.
pub(crate) async fn receiver_entry_point(
    timeout: Duration,
    stream: impl AsyncBufReadExt + std::marker::Unpin,
) -> anyhow::Result<()> {
    // Parse structured crash data stream into configuration and crash information
    if let Some((config, mut crash_info)) = receive_report_from_stream(timeout, stream).await? {
        // Attempt symbol resolution - errors are accumulated, not fatal
        if let Err(e) = resolve_frames(&config, &mut crash_info) {
            crash_info
                .log_messages
                .push(format!("Error resolving frames: {e}"));
        }

        // Attempt name demangling if enabled - errors are accumulated, not fatal
        if config.demangle_names() {
            if let Err(e) = crash_info.demangle_names() {
                crash_info
                    .log_messages
                    .push(format!("Error demangling names: {e}"));
            }
        }

        // Upload formatted crash report to backend endpoint
        crash_info
            .async_upload_to_endpoint(config.endpoint())
            .await?;
    }
    Ok(())
}

fn receiver_timeout() -> Duration {
    // https://github.com/DataDog/libdatadog/issues/717
    if let Ok(s) = std::env::var("DD_CRASHTRACKER_RECEIVER_TIMEOUT_MS") {
        if let Ok(v) = s.parse() {
            return Duration::from_millis(v);
        }
    }
    // Default value
    Duration::from_millis(4000)
}

fn resolve_frames(
    config: &CrashtrackerConfiguration,
    crash_info: &mut CrashInfo,
) -> anyhow::Result<()> {
    if config.resolve_frames() == StacktraceCollection::EnabledWithSymbolsInReceiver {
        let pid = crash_info
            .proc_info
            .as_ref()
            .context("Unable to resolve frames: No PID specified")?
            .pid;
        let rval1 = crash_info.resolve_names(pid);
        let rval2 = crash_info.normalize_ips(pid);
        anyhow::ensure!(
            rval1.is_ok() && rval2.is_ok(),
            "resolve_names: {rval1:?}\tnormalize_ips: {rval2:?}"
        );
    }
    Ok(())
}
