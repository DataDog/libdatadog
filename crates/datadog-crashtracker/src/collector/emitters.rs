// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::collector::additional_tags::consume_and_emit_additional_tags;
use crate::collector::counters::emit_counters;
use crate::collector::spans::{emit_spans, emit_traces};
use crate::shared::constants::*;
use crate::{translate_si_code, CrashtrackerConfiguration, SignalNames, StacktraceCollection};
use anyhow::Context;
use backtrace::Frame;
use libc::{siginfo_t, ucontext_t};
use std::{
    fs::File,
    io::{Read, Write},
};

/// Emit a stacktrace onto the given handle as formatted json.
/// SAFETY:
///     Crash-tracking functions are not reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function is not atomic. A crash during its execution may lead to
///     unexpected crash-handling behaviour.
/// SIGNAL SAFETY:
///     Getting a backtrace on rust is not guaranteed to be signal safe.
///     https://github.com/rust-lang/backtrace-rs/issues/414
///     Calculating the `ip` of the frames seems safe, but resolving the frames
///     sometimes crashes.
unsafe fn emit_backtrace_by_frames(
    w: &mut impl Write,
    resolve_frames: StacktraceCollection,
) -> anyhow::Result<()> {
    // https://docs.rs/backtrace/latest/backtrace/index.html
    writeln!(w, "{DD_CRASHTRACK_BEGIN_STACKTRACE}")?;

    // Absolute addresses appear to be safer to collect during a crash than debug info.
    fn emit_absolute_addresses(w: &mut impl Write, frame: &Frame) -> anyhow::Result<()> {
        write!(w, "\"ip\": \"{:?}\"", frame.ip())?;
        if let Some(module_base_address) = frame.module_base_address() {
            write!(w, ", \"module_base_address\": \"{module_base_address:?}\"",)?;
        }
        write!(w, ", \"sp\": \"{:?}\"", frame.sp())?;
        write!(w, ", \"symbol_address\": \"{:?}\"", frame.symbol_address())?;
        Ok(())
    }

    backtrace::trace_unsynchronized(|frame| {
        if resolve_frames == StacktraceCollection::EnabledWithInprocessSymbols {
            backtrace::resolve_frame_unsynchronized(frame, |symbol| {
                write!(w, "{{").unwrap();
                #[allow(clippy::unwrap_used)]
                emit_absolute_addresses(w, frame).unwrap();
                if let Some(column) = symbol.colno() {
                    write!(w, ", \"column\": {column}").unwrap();
                }
                if let Some(file) = symbol.filename() {
                    // The debug printer for path already wraps it in `"` marks.
                    write!(w, ", \"file\": {file:?}").unwrap();
                }
                if let Some(function) = symbol.name() {
                    write!(w, ", \"function\": \"{function}\"").unwrap();
                }
                if let Some(line) = symbol.lineno() {
                    write!(w, ", \"line\": {line}").unwrap();
                }
                writeln!(w, "}}").unwrap();
                // Flush eagerly to ensure that each frame gets emitted even if the next one fails
                #[allow(clippy::unwrap_used)]
                w.flush().unwrap();
            });
        } else {
            write!(w, "{{").unwrap();
            #[allow(clippy::unwrap_used)]
            emit_absolute_addresses(w, frame).unwrap();
            writeln!(w, "}}").unwrap();
            // Flush eagerly to ensure that each frame gets emitted even if the next one fails
            #[allow(clippy::unwrap_used)]
            w.flush().unwrap();
        }
        true // keep going to the next frame
    });
    #[allow(clippy::unwrap_used)]
    writeln!(w, "{DD_CRASHTRACK_END_STACKTRACE}").unwrap();
    w.flush()?;
    Ok(())
}

pub(crate) fn emit_crashreport(
    pipe: &mut impl Write,
    config: &CrashtrackerConfiguration,
    config_str: &str,
    metadata_string: &str,
    sig_info: *const siginfo_t,
    ucontext: *const ucontext_t,
) -> anyhow::Result<()> {
    emit_metadata(pipe, metadata_string)?;
    emit_config(pipe, config_str)?;
    emit_siginfo(pipe, sig_info)?;
    emit_ucontext(pipe, ucontext)?;
    emit_procinfo(pipe)?;
    emit_counters(pipe)?;
    emit_spans(pipe)?;
    consume_and_emit_additional_tags(pipe)?;
    emit_traces(pipe)?;

    #[cfg(target_os = "linux")]
    emit_proc_self_maps(pipe)?;

    // Getting a backtrace on rust is not guaranteed to be signal safe
    // https://github.com/rust-lang/backtrace-rs/issues/414
    // let current_backtrace = backtrace::Backtrace::new();
    // In fact, if we look into the code here, we see mallocs.
    // https://doc.rust-lang.org/src/std/backtrace.rs.html#332
    // Do this last, so even if it crashes, we still get the other info.
    if config.resolve_frames() != StacktraceCollection::Disabled {
        unsafe { emit_backtrace_by_frames(pipe, config.resolve_frames())? };
    }
    writeln!(pipe, "{DD_CRASHTRACK_DONE}")?;
    pipe.flush()?;

    Ok(())
}

fn emit_config(w: &mut impl Write, config_str: &str) -> anyhow::Result<()> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_CONFIG}")?;
    writeln!(w, "{}", config_str)?;
    writeln!(w, "{DD_CRASHTRACK_END_CONFIG}")?;
    w.flush()?;
    Ok(())
}

fn emit_metadata(w: &mut impl Write, metadata_str: &str) -> anyhow::Result<()> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_METADATA}")?;
    writeln!(w, "{}", metadata_str)?;
    writeln!(w, "{DD_CRASHTRACK_END_METADATA}")?;
    w.flush()?;
    Ok(())
}

fn emit_procinfo(w: &mut impl Write) -> anyhow::Result<()> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_PROCINFO}")?;
    let pid = nix::unistd::getpid();
    writeln!(w, "{{\"pid\": {pid} }}")?;
    writeln!(w, "{DD_CRASHTRACK_END_PROCINFO}")?;
    w.flush()?;
    Ok(())
}

#[cfg(target_os = "linux")]
/// `/proc/self/maps` is very useful for debugging, and difficult to get from
/// the child process (permissions issues on Linux).  Emit it directly onto the
/// pipe to get around this.
fn emit_proc_self_maps(w: &mut impl Write) -> anyhow::Result<()> {
    emit_text_file(w, "/proc/self/maps")?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn emit_ucontext(w: &mut impl Write, ucontext: *const ucontext_t) -> anyhow::Result<()> {
    anyhow::ensure!(!ucontext.is_null());
    writeln!(w, "{DD_CRASHTRACK_BEGIN_UCONTEXT}")?;
    // SAFETY: the pointer is given to us by the signal handler, and is non-null.
    writeln!(w, "{:?}", unsafe { *ucontext })?;
    writeln!(w, "{DD_CRASHTRACK_END_UCONTEXT}")?;
    w.flush()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn emit_ucontext(w: &mut impl Write, ucontext: *const ucontext_t) -> anyhow::Result<()> {
    anyhow::ensure!(!ucontext.is_null());
    // On MacOS, the actual machine context is behind a second pointer.
    // SAFETY: the pointer is given to us by the signal handler, and is non-null.
    let mcontext = unsafe { *ucontext }.uc_mcontext;
    writeln!(w, "{DD_CRASHTRACK_BEGIN_UCONTEXT}")?;
    // SAFETY: the pointer is given to us by the signal handler, and is non-null.
    write!(w, "{:?}", unsafe { *ucontext })?;
    if !mcontext.is_null() {
        // SAFETY: the pointer is given to us by the signal handler, and is non-null.
        write!(w, ", {:?}", unsafe { *mcontext })?;
    }
    writeln!(w)?;
    writeln!(w, "{DD_CRASHTRACK_END_UCONTEXT}")?;
    w.flush()?;
    Ok(())
}

fn emit_siginfo(w: &mut impl Write, sig_info: *const siginfo_t) -> anyhow::Result<()> {
    anyhow::ensure!(!sig_info.is_null());

    let si_signo = unsafe { (*sig_info).si_signo };
    let si_signo_human_readable: SignalNames = si_signo.into();

    // Derive the faulting address from `sig_info`
    // https://man7.org/linux/man-pages/man2/sigaction.2.html
    // SIGILL, SIGFPE, SIGSEGV, SIGBUS, and SIGTRAP fill in si_addr with the address of the fault.
    let si_addr: Option<usize> = match si_signo {
        libc::SIGILL | libc::SIGFPE | libc::SIGSEGV | libc::SIGBUS | libc::SIGTRAP => {
            Some(unsafe { (*sig_info).si_addr() as usize })
        }
        _ => None,
    };

    let si_code = unsafe { (*sig_info).si_code };
    let si_code_human_readable = translate_si_code(si_signo, si_code);

    writeln!(w, "{DD_CRASHTRACK_BEGIN_SIGINFO}")?;
    write!(w, "{{")?;
    write!(w, "\"si_code\": {si_code}")?;
    write!(
        w,
        ", \"si_code_human_readable\": \"{si_code_human_readable:?}\""
    )?;
    write!(w, ", \"si_signo\": {si_signo}")?;
    write!(
        w,
        ", \"si_signo_human_readable\": \"{si_signo_human_readable:?}\""
    )?;
    if let Some(si_addr) = si_addr {
        write!(w, ", \"si_addr\": \"{si_addr:#018x}\"")?;
    }
    writeln!(w, "}}")?;
    writeln!(w, "{DD_CRASHTRACK_END_SIGINFO}")?;
    w.flush()?;
    Ok(())
}

/// Emit a file onto the given handle.
/// The file will be emitted in the format
///
/// DD_CRASHTRACK_BEGIN_FILE
/// <FILE BYTES>
/// DD_CRASHTRACK_END_FILE
///
/// PRECONDITIONS:
///     This function assumes that the crash-tracker is initialized.
///     The receiver expects the file to contain valid UTF-8 compatible text.
/// SAFETY:
///     Crash-tracking functions are not reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function is not atomic. A crash during its execution may lead to
///     unexpected crash-handling behaviour.
/// SIGNAL SAFETY:
///     This function is careful to only write to the handle, without doing any
///     unnecessary mutexes or memory allocation.
#[allow(dead_code)]
fn emit_text_file(w: &mut impl Write, path: &str) -> anyhow::Result<()> {
    // open is signal safe
    // https://man7.org/linux/man-pages/man7/signal-safety.7.html
    let mut file = File::open(path).with_context(|| path.to_string())?;

    // Reading the file into a fixed buffer is signal safe.
    // Doing anything more complicated may involve allocation which is not.
    // So, just read it in, and then immediately push it out to the pipe.
    const BUFFER_LEN: usize = 512;
    let mut buffer = [0u8; BUFFER_LEN];

    writeln!(w, "{DD_CRASHTRACK_BEGIN_FILE} {path}")?;

    loop {
        let read_count = file.read(&mut buffer)?;
        w.write_all(&buffer[..read_count])?;
        if read_count == 0 {
            break;
        }
    }
    writeln!(w, "\n{DD_CRASHTRACK_END_FILE} \"{path}\"")?;
    w.flush()?;
    Ok(())
}
