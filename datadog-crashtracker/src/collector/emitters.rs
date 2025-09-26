// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::collector::additional_tags::consume_and_emit_additional_tags;
use crate::collector::counters::emit_counters;
use crate::collector::spans::{emit_spans, emit_traces};
use crate::shared::constants::*;
use crate::{translate_si_code, CrashtrackerConfiguration, SignalNames, StacktraceCollection};
use backtrace::Frame;
use libc::{siginfo_t, ucontext_t};
use std::{
    fs::File,
    io::{Read, Write},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EmitterError {
    #[error("Failed to write to output: {0}")]
    WriteError(#[from] std::io::Error),
    #[error("Failed to open file: {0}")]
    FileOpenError(std::io::Error),
    #[error("Null pointer provided for ucontext")]
    NullUcontext,
    #[error("Null pointer provided for siginfo")]
    NullSiginfo,
    #[error("Counter error: {0}")]
    CounterError(#[from] crate::collector::counters::CounterError),
    #[error("Atomic set error: {0}")]
    AtomicSetError(#[from] crate::collector::atomic_set::AtomicSetError),
}

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
    fault_rsp: usize,
) -> Result<(), EmitterError> {
    // https://docs.rs/backtrace/latest/backtrace/index.html
    writeln!(w, "{DD_CRASHTRACK_BEGIN_STACKTRACE}")?;

    // Absolute addresses appear to be safer to collect during a crash than debug info.
    fn emit_absolute_addresses(w: &mut impl Write, frame: &Frame) -> Result<(), EmitterError> {
        write!(w, "\"ip\": \"{:?}\"", frame.ip())?;
        if let Some(module_base_address) = frame.module_base_address() {
            write!(w, ", \"module_base_address\": \"{module_base_address:?}\"",)?;
        }
        write!(w, ", \"sp\": \"{:?}\"", frame.sp())?;
        write!(w, ", \"symbol_address\": \"{:?}\"", frame.symbol_address())?;
        Ok(())
    }

    backtrace::trace_unsynchronized(|frame| {
        // Skip all stack frames whose stack pointer is less than to the determined crash stack
        // pointer (fault_rsp). These frames belong exclusively to the crash tracker and the
        // backtrace functionality and are therefore not relevant for troubleshooting.
        let sp = frame.sp();
        if !sp.is_null() && (sp as usize) < fault_rsp {
            return true;
        }
        if resolve_frames == StacktraceCollection::EnabledWithInprocessSymbols {
            backtrace::resolve_frame_unsynchronized(frame, |symbol| {
                // In crash handling context, we need to be resilient to write failures
                // If any write fails, we continue to try the next operations
                let _ = write!(w, "{{");
                let _ = emit_absolute_addresses(w, frame);
                if let Some(column) = symbol.colno() {
                    let _ = write!(w, ", \"column\": {column}");
                }
                if let Some(file) = symbol.filename() {
                    // The debug printer for path already wraps it in `"` marks.
                    let _ = write!(w, ", \"file\": {file:?}");
                }
                if let Some(function) = symbol.name() {
                    let _ = write!(w, ", \"function\": \"{function}\"");
                }
                if let Some(line) = symbol.lineno() {
                    let _ = write!(w, ", \"line\": {line}");
                }
                let _ = writeln!(w, "}}");
                // Flush eagerly to ensure that each frame gets emitted even if the next one fails
                let _ = w.flush();
            });
        } else {
            // In crash handling context, we need to be resilient to write failures
            let _ = write!(w, "{{");
            let _ = emit_absolute_addresses(w, frame);
            let _ = writeln!(w, "}}");
            // Flush eagerly to ensure that each frame gets emitted even if the next one fails
            let _ = w.flush();
        }
        true // keep going to the next frame
    });
    writeln!(w, "{DD_CRASHTRACK_END_STACKTRACE}")?;
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
    ppid: i32,
) -> Result<(), EmitterError> {
    emit_metadata(pipe, metadata_string)?;
    emit_config(pipe, config_str)?;
    emit_siginfo(pipe, sig_info)?;
    emit_ucontext(pipe, ucontext)?;
    emit_procinfo(pipe, ppid)?;
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
        let fault_rsp = extract_rsp(ucontext);
        unsafe { emit_backtrace_by_frames(pipe, config.resolve_frames(), fault_rsp)? };
    }
    writeln!(pipe, "{DD_CRASHTRACK_DONE}")?;
    pipe.flush()?;

    Ok(())
}

fn emit_config(w: &mut impl Write, config_str: &str) -> Result<(), EmitterError> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_CONFIG}")?;
    writeln!(w, "{config_str}")?;
    writeln!(w, "{DD_CRASHTRACK_END_CONFIG}")?;
    w.flush()?;
    Ok(())
}

fn emit_metadata(w: &mut impl Write, metadata_str: &str) -> Result<(), EmitterError> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_METADATA}")?;
    writeln!(w, "{metadata_str}")?;
    writeln!(w, "{DD_CRASHTRACK_END_METADATA}")?;
    w.flush()?;
    Ok(())
}

fn emit_procinfo(w: &mut impl Write, pid: i32) -> Result<(), EmitterError> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_PROCINFO}")?;
    writeln!(w, "{{\"pid\": {pid} }}")?;
    writeln!(w, "{DD_CRASHTRACK_END_PROCINFO}")?;
    w.flush()?;
    Ok(())
}

#[cfg(target_os = "linux")]
/// Assumes that the memory layout of the current process (child) is identical to
/// the layout of the target process (parent), which should always be true.
fn emit_proc_self_maps(w: &mut impl Write) -> Result<(), EmitterError> {
    emit_text_file(w, "/proc/self/maps")?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn emit_ucontext(w: &mut impl Write, ucontext: *const ucontext_t) -> Result<(), EmitterError> {
    if ucontext.is_null() {
        return Err(EmitterError::NullUcontext);
    }
    writeln!(w, "{DD_CRASHTRACK_BEGIN_UCONTEXT}")?;
    // SAFETY: the pointer is given to us by the signal handler, and is non-null.
    writeln!(w, "{:?}", unsafe { *ucontext })?;
    writeln!(w, "{DD_CRASHTRACK_END_UCONTEXT}")?;
    w.flush()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn emit_ucontext(w: &mut impl Write, ucontext: *const ucontext_t) -> Result<(), EmitterError> {
    if ucontext.is_null() {
        return Err(EmitterError::NullUcontext);
    }
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

fn emit_siginfo(w: &mut impl Write, sig_info: *const siginfo_t) -> Result<(), EmitterError> {
    if sig_info.is_null() {
        return Err(EmitterError::NullSiginfo);
    }

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
fn emit_text_file(w: &mut impl Write, path: &str) -> Result<(), EmitterError> {
    // open is signal safe
    // https://man7.org/linux/man-pages/man7/signal-safety.7.html
    let mut file = File::open(path).map_err(EmitterError::FileOpenError)?;

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

fn extract_rsp(ucontext: *const ucontext_t) -> usize {
    unsafe {
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        return (*(*ucontext).uc_mcontext).__ss.__rsp as usize;
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        return (*(*ucontext).uc_mcontext).__ss.__sp as usize;

        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        return (*ucontext).uc_mcontext.gregs[libc::REG_RSP as usize] as usize;
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        return (*ucontext).uc_mcontext.sp as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_emit_backtrace_disabled() {
        let mut buf = Vec::new();
        unsafe {
            emit_backtrace_by_frames(&mut buf, StacktraceCollection::Disabled, 0)
                .expect("to work ;-)");
        }
        let out = str::from_utf8(&buf).expect("to be valid UTF8");
        assert!(out.contains("BEGIN_STACKTRACE"));
        assert!(out.contains("END_STACKTRACE"));
        assert!(out.contains("\"ip\":"));
        assert!(
            !out.contains("\"column\":"),
            "'column' key must not be emitted"
        );
        assert!(!out.contains("\"file\":"), "'file' key must not be emitted");
        assert!(
            !out.contains("\"function\":"),
            "'function' key must not be emitted"
        );
        assert!(!out.contains("\"line\":"), "'line' key must not be emitted");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_emit_backtrace_with_symbols() {
        let dummy = 0u8;
        // retrieve stack pointer for this function
        let sp_of_test_fn = &dummy as *const u8 as usize;
        let mut buf = Vec::new();
        unsafe {
            emit_backtrace_by_frames(
                &mut buf,
                StacktraceCollection::EnabledWithInprocessSymbols,
                sp_of_test_fn,
            )
            .expect("to work ;-)");
        }
        let out = str::from_utf8(&buf).expect("to be valid UTF8");
        assert!(out.contains("BEGIN_STACKTRACE"));
        assert!(out.contains("END_STACKTRACE"));
        // basic structure assertions
        assert!(out.contains("\"column\":"), "'column' key missing");
        assert!(out.contains("\"file\":"), "'file' key missing");
        assert!(out.contains("\"function\":"), "'function' key missing");
        assert!(out.contains("\"line\":"), "'line' key missing");
        // filter assertions
        assert!(
            !out.contains("emitters::emit_backtrace_by_frames"),
            "crashtracker itself must be filtered, found 'backtrace::backtrace::libunwind'"
        );
        assert!(
            !out.contains("backtrace::backtrace"),
            "crashtracker itself must be filtered away, found 'backtrace::backtrace'"
        );
    }
}
