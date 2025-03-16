// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::receive_report::receive_report_from_stream;
use crate::{crash_info::CrashInfo, CrashtrackerConfiguration, StacktraceCollection};
use anyhow::Context;
#[cfg(feature ="pyo3")]
use pyo3::prelude::*;
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::UnixListener,
};

/*-----------------------------------------
|                Public API               |
------------------------------------------*/
#[cfg_attr(feature = "pyo3", pyfunction)]
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
    if let Some((config, mut crash_info)) = receive_report_from_stream(timeout, stream).await? {
        if let Err(e) = resolve_frames(&config, &mut crash_info) {
            crash_info
                .log_messages
                .push(format!("Error resolving frames: {e}"));
        }
        crash_info
            .async_upload_to_endpoint(&config.endpoint)
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
    if config.resolve_frames == StacktraceCollection::EnabledWithSymbolsInReceiver {
        let pid = crash_info
            .proc_info
            .as_ref()
            .context("Unable to resolve frames: No PID specified")?
            .pid;
        crash_info.resolve_names(pid)?;
        crash_info.normalize_ips(pid)?;
    }
    Ok(())
}
