// SPDX-License-Identifier: Apache-2.0
// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/

use super::receive_report::receive_report_from_stream;
use crate::crash_info::CrashInfo;
use crate::CrashtrackerConfiguration;
#[cfg(target_os = "linux")]
use crate::StacktraceCollection;
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

pub async fn async_receiver_entry_point_stream(
    stream: impl AsyncBufReadExt + std::marker::Unpin,
) -> anyhow::Result<()> {
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

/// Receives data from a crash collector via a stream, formats it into
/// `CrashInfo` json, and emits it to the endpoint/file defined in `config`.
///
/// At a high-level, this exists because doing anything in a
/// signal handler is dangerous, so we fork a sidecar to do the stuff we aren't
/// allowed to do in the handler.
///
/// See comments in [libdd-crashtracker/lib.rs] for a full architecture
/// description.
pub(crate) async fn receiver_entry_point(
    timeout: Duration,
    mut stream: impl AsyncBufReadExt + std::marker::Unpin,
) -> anyhow::Result<()> {
    if let Some((config, mut crash_info)) = receive_report_from_stream(timeout, &mut stream).await?
    {
        // Symbolization reads /proc/<pid>/maps, and the crashing process is
        // waiting on POLLHUP from this connection to terminate. Hold it open
        // until the report is symbolized, then release the process before the
        // upload so a slow endpoint does not extend the crash pause.
        if let Err(e) = resolve_frames(&config, &mut crash_info) {
            crash_info
                .log_messages
                .push(format!("Error resolving frames: {e}"));
        }
        drop(stream);

        if config.demangle_names() {
            if let Err(e) = crash_info.demangle_names() {
                crash_info
                    .log_messages
                    .push(format!("Error demangling names: {e}"));
            }
        }
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
    // enrich_callstacks uses blazesym's normalize_user_addrs (reads /proc/<pid>/maps)
    // and assumes ELF binaries. Both are Linux-specific; macOS has no procfs and
    // uses Mach-O binaries.
    #[cfg(target_os = "linux")]
    if config.resolve_frames() == StacktraceCollection::EnabledWithSymbolsInReceiver {
        let pid = crash_info
            .proc_info
            .as_ref()
            .context("Unable to resolve frames: No PID specified")?
            .pid;
        let enrichment = crash_info.enrich_callstacks(pid);
        finish_native_stacks(crash_info);
        enrichment?;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (config, crash_info);
    Ok(())
}

#[cfg(target_os = "linux")]
fn finish_native_stacks(crash_info: &mut CrashInfo) {
    // A software-generated fatal signal is captured while raise/pthread_kill is
    // still on the stack. Those libc delivery frames describe how the signal
    // arrived, not what application operation requested it. Remove only the
    // contiguous leading libc segment and leave the first application frame as
    // the grouping frame.
    if crash_info
        .sig_info
        .as_ref()
        .is_some_and(|sig_info| sig_info.si_code <= 0)
    {
        trim_signal_delivery_frames(&mut crash_info.error.stack);
    }

    synthesize_module_offsets(&mut crash_info.error.stack);
    if let Some(threads) = crash_info.error.threads.as_mut() {
        for thread in threads {
            synthesize_module_offsets(&mut thread.stack);
        }
    }
}

#[cfg(target_os = "linux")]
fn trim_signal_delivery_frames(stack: &mut crate::crash_info::StackTrace) {
    let delivery_frames = stack
        .frames
        .iter()
        .take_while(|frame| {
            frame.path.as_deref().is_some_and(|path| {
                let basename = std::path::Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(path);
                basename == "libc.so.6"
                    || basename.starts_with("libc-")
                    || basename.starts_with("ld-musl-")
            })
        })
        .count();
    stack.frames.drain(..delivery_frames);
}

#[cfg(target_os = "linux")]
fn synthesize_module_offsets(stack: &mut crate::crash_info::StackTrace) {
    for frame in &mut stack.frames {
        if frame.function.is_some() {
            continue;
        }
        let (Some(path), Some(relative_address)) =
            (frame.path.as_deref(), frame.relative_address.as_deref())
        else {
            continue;
        };
        let module = std::path::Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(path);
        frame.function = Some(format!("{module}+{relative_address}"));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::crash_info::{StackFrame, StackTrace};

    fn frame(path: &str, relative_address: &str, function: Option<&str>) -> StackFrame {
        StackFrame {
            path: Some(path.to_string()),
            relative_address: Some(relative_address.to_string()),
            function: function.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn trims_only_leading_signal_delivery_frames() {
        let mut stack = StackTrace::from_frames(
            vec![
                frame("/lib/ld-musl-x86_64.so.1", "0x1", Some("raise")),
                frame("/opt/app", "0x2", Some("main")),
                frame("/lib/libc.so.6", "0x3", Some("__libc_start_main")),
            ],
            false,
        );

        trim_signal_delivery_frames(&mut stack);

        assert_eq!(stack.frames.len(), 2);
        assert_eq!(stack.frames[0].function.as_deref(), Some("main"));
        assert_eq!(
            stack.frames[1].function.as_deref(),
            Some("__libc_start_main")
        );
    }

    #[test]
    fn adds_module_offset_only_when_symbolization_has_no_name() {
        let mut stack = StackTrace::from_frames(
            vec![
                frame("/usr/lib/libcupti.so.12", "0x0000000000001234", None),
                frame("/opt/app", "0x2", Some("main")),
            ],
            false,
        );

        synthesize_module_offsets(&mut stack);

        assert_eq!(
            stack.frames[0].function.as_deref(),
            Some("libcupti.so.12+0x0000000000001234")
        );
        assert_eq!(stack.frames[1].function.as_deref(), Some("main"));
    }
}
