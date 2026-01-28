// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::collector::additional_tags::consume_and_emit_additional_tags;
use crate::collector::counters::emit_counters;
use crate::collector::spans::{emit_spans, emit_traces};
use crate::runtime_callback::{
    get_registered_callback, invoke_runtime_callback_with_writer, is_runtime_callback_registered,
    CallbackData,
};
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
    fault_ip: usize,
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

    let mut ip_found = false;
    loop {
        backtrace::trace_unsynchronized(|frame| {
            // Skip all stack frames until we encounter the determined crash instruction pointer
            // (fault_ip). These initial frames belong exclusively to the crash tracker and the
            // backtrace functionality and are therefore not relevant for troubleshooting.
            let ip = frame.ip();
            if ip as usize == fault_ip {
                ip_found = true;
            }
            if !ip_found {
                return true;
            }
            if resolve_frames == StacktraceCollection::EnabledWithInprocessSymbols {
                backtrace::resolve_frame_unsynchronized(frame, |symbol| {
                    #[allow(clippy::unwrap_used)]
                    write!(w, "{{").unwrap();
                    #[allow(clippy::unwrap_used)]
                    emit_absolute_addresses(w, frame).unwrap();
                    if let Some(column) = symbol.colno() {
                        #[allow(clippy::unwrap_used)]
                        write!(w, ", \"column\": {column}").unwrap();
                    }
                    if let Some(file) = symbol.filename() {
                        // The debug printer for path already wraps it in `"` marks.
                        #[allow(clippy::unwrap_used)]
                        write!(w, ", \"file\": {file:?}").unwrap();
                    }
                    if let Some(function) = symbol.name() {
                        #[allow(clippy::unwrap_used)]
                        write!(w, ", \"function\": \"{function}\"").unwrap();
                    }
                    if let Some(line) = symbol.lineno() {
                        #[allow(clippy::unwrap_used)]
                        write!(w, ", \"line\": {line}").unwrap();
                    }
                    #[allow(clippy::unwrap_used)]
                    writeln!(w, "}}").unwrap();
                    // Flush eagerly to ensure that each frame gets emitted even if the next one
                    // fails
                    #[allow(clippy::unwrap_used)]
                    w.flush().unwrap();
                });
            } else {
                #[allow(clippy::unwrap_used)]
                write!(w, "{{").unwrap();
                #[allow(clippy::unwrap_used)]
                emit_absolute_addresses(w, frame).unwrap();
                #[allow(clippy::unwrap_used)]
                writeln!(w, "}}").unwrap();
                // Flush eagerly to ensure that each frame gets emitted even if the next one fails
                #[allow(clippy::unwrap_used)]
                w.flush().unwrap();
            }
            true // keep going to the next frame
        });
        if ip_found {
            break;
        }
        // emit anything at all, if the crashing frame is not found for some reason
        ip_found = true;
    }
    writeln!(w, "{DD_CRASHTRACK_END_STACKTRACE}")?;
    w.flush()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_crashreport(
    pipe: &mut impl Write,
    config: &CrashtrackerConfiguration,
    config_str: &str,
    metadata_string: &str,
    message_ptr: *mut String,
    sig_info: *const siginfo_t,
    ucontext: *const ucontext_t,
    ppid: i32,
    crashing_tid: libc::pid_t,
) -> Result<(), EmitterError> {
    // The following order is important in order to emit the crash ping:
    // - receiver expects the config
    // - then message if any
    // - then siginfo (if the message is not set, we use the siginfo to generate the message)
    // - then metadata
    emit_config(pipe, config_str)?;
    emit_message(pipe, message_ptr)?;
    emit_siginfo(pipe, sig_info)?;
    emit_metadata(pipe, metadata_string)?;
    // after the metadata the ping should have been sent
    emit_ucontext(pipe, ucontext)?;
    emit_procinfo(pipe, ppid)?;
    emit_counters(pipe)?;
    emit_spans(pipe)?;
    consume_and_emit_additional_tags(pipe)?;
    emit_traces(pipe)?;

    #[cfg(target_os = "linux")]
    emit_proc_self_maps(pipe)?;
    #[cfg(target_os = "linux")]
    emit_thread_name(pipe, ppid, crashing_tid)?;

    // Getting a backtrace on rust is not guaranteed to be signal safe
    // https://github.com/rust-lang/backtrace-rs/issues/414
    // let current_backtrace = backtrace::Backtrace::new();
    // In fact, if we look into the code here, we see mallocs.
    // https://doc.rust-lang.org/src/std/backtrace.rs.html#332
    // Do this last, so even if it crashes, we still get the other info.
    if config.resolve_frames() != StacktraceCollection::Disabled {
        let fault_ip = extract_ip(ucontext);
        unsafe { emit_backtrace_by_frames(pipe, config.resolve_frames(), fault_ip)? };
    }

    if is_runtime_callback_registered() {
        emit_runtime_stack(pipe)?;
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

fn emit_message(w: &mut impl Write, message_ptr: *mut String) -> Result<(), EmitterError> {
    if !message_ptr.is_null() {
        let message = unsafe { &*message_ptr };
        if !message.trim().is_empty() {
            writeln!(w, "{DD_CRASHTRACK_BEGIN_MESSAGE}")?;
            writeln!(w, "{message}")?;
            writeln!(w, "{DD_CRASHTRACK_END_MESSAGE}")?;
            w.flush()?;
        }
    }
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

/// Assumes that the memory layout of the current process (child) is identical to
/// the layout of the target process (parent), which should always be true.
#[cfg(target_os = "linux")]
fn emit_thread_name(
    w: &mut impl Write,
    pid: i32,
    crashing_tid: libc::pid_t,
) -> Result<(), EmitterError> {
    // Best effort at string formatting with no async signal un-safe calls
    // Format: /proc/{pid}/task/{tid}/comm\0
    fn write_decimal(buf: &mut [u8], mut val: u64) -> Option<usize> {
        if buf.is_empty() {
            return None;
        }
        let mut i = 0;
        loop {
            if i >= buf.len() {
                return None;
            }
            buf[i] = b'0' + (val % 10) as u8;
            val /= 10;
            i += 1;
            if val == 0 {
                break;
            }
        }
        buf[..i].reverse();
        Some(i)
    }

    let mut path_buf = [0u8; 64];
    let mut idx = 0usize;
    let parts = [
        b"/proc/" as &[u8],
        b"" as &[u8], // pid placeholder
        b"/task/" as &[u8],
        b"" as &[u8], // tid placeholder
        b"/comm" as &[u8],
    ];

    // Copy "/proc/"
    path_buf[idx..idx + parts[0].len()].copy_from_slice(parts[0]);
    idx += parts[0].len();
    // pid
    let pid_start = idx;
    if let Some(len) = write_decimal(&mut path_buf[pid_start..], pid as u64) {
        idx += len;
    } else {
        return Ok(());
    }
    // "/task/"
    path_buf[idx..idx + parts[2].len()].copy_from_slice(parts[2]);
    idx += parts[2].len();
    // tid
    let tid_start = idx;
    if let Some(len) = write_decimal(&mut path_buf[tid_start..], crashing_tid as u64) {
        idx += len;
    } else {
        return Ok(());
    }
    // "/comm"
    path_buf[idx..idx + parts[4].len()].copy_from_slice(parts[4]);
    idx += parts[4].len();
    // null-terminate
    if idx >= path_buf.len() {
        return Ok(());
    }
    path_buf[idx] = 0;

    let fd = unsafe { libc::open(path_buf.as_ptr() as *const libc::c_char, libc::O_RDONLY) };
    if fd < 0 {
        // Missing / unreadable; skip without failing the crash report.
        return Ok(());
    }

    writeln!(w, "{DD_CRASHTRACK_BEGIN_THREAD_NAME}")?;

    const BUFFER_LEN: usize = 64;
    let mut buffer = [0u8; BUFFER_LEN];

    loop {
        let read_count =
            unsafe { libc::read(fd, buffer.as_mut_ptr() as *mut libc::c_void, BUFFER_LEN) };
        if read_count <= 0 {
            break;
        }
        w.write_all(&buffer[..read_count as usize])?;
    }

    // Best-effort close
    let _ = unsafe { libc::close(fd) };

    writeln!(w, "{DD_CRASHTRACK_END_THREAD_NAME}")?;
    w.flush()?;
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

/// Emit runtime stack frames collected from registered runtime callback
///
/// This function invokes any registered runtime callback to collect runtime-specific
/// stack traces
///
/// If runtime stacks are being emitted frame by frame, this function writes structured JSON.
/// If not, it writes a single line with the stacktrace string.
///
/// SAFETY:
///     Crash-tracking functions are not reentrant.
///     No other crash-handler functions should be called concurrently.
/// SIGNAL SAFETY:
///     This function attempts to be signal safe by only invoking user-registered
///     callbacks and writing to the provided stream. The runtime callback itself
///     must be signal safe.
fn emit_runtime_stack(w: &mut impl Write) -> Result<(), EmitterError> {
    let callback = unsafe { get_registered_callback() };

    let callback = match callback {
        Some(callback) => callback,
        None => return Ok(()),
    };

    match callback {
        CallbackData::Frame(_) => emit_runtime_stack_by_frames(w),
        CallbackData::StacktraceString(_) => emit_runtime_stack_by_stacktrace_string(w),
    }
}

fn emit_runtime_stack_by_frames(w: &mut impl Write) -> Result<(), EmitterError> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_RUNTIME_STACK_FRAME}")?;
    unsafe { invoke_runtime_callback_with_writer(w)? };
    writeln!(w, "{DD_CRASHTRACK_END_RUNTIME_STACK_FRAME}")?;
    w.flush()?;
    Ok(())
}

fn emit_runtime_stack_by_stacktrace_string(w: &mut impl Write) -> Result<(), EmitterError> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_RUNTIME_STACK_STRING}")?;
    unsafe { invoke_runtime_callback_with_writer(w)? };
    writeln!(w, "{DD_CRASHTRACK_END_RUNTIME_STACK_STRING}")?;
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

fn extract_ip(ucontext: *const ucontext_t) -> usize {
    unsafe {
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        return (*(*ucontext).uc_mcontext).__ss.__rip as usize;
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        return (*(*ucontext).uc_mcontext).__ss.__pc as usize;

        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        return (*ucontext).uc_mcontext.gregs[libc::REG_RIP as usize] as usize;
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        return (*ucontext).uc_mcontext.pc as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str;

    #[inline(never)]
    fn inner_test_emit_backtrace_with_symbols(collection: StacktraceCollection) -> Vec<u8> {
        let mut ip_of_test_fn = 0;
        let mut skip = 3;
        unsafe {
            backtrace::trace_unsynchronized(|frame| {
                ip_of_test_fn = frame.ip() as usize;
                skip -= 1;
                skip > 0
            })
        };
        let mut buf = Vec::new();
        unsafe {
            emit_backtrace_by_frames(&mut buf, collection, ip_of_test_fn).expect("to work ;-)");
        }
        buf
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_emit_backtrace_disabled() {
        let buf = inner_test_emit_backtrace_with_symbols(StacktraceCollection::Disabled);
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
        let buf = inner_test_emit_backtrace_with_symbols(
            StacktraceCollection::EnabledWithInprocessSymbols,
        );
        // retrieve stack pointer for this function
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

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_emit_message_nullptr() {
        let mut buf = Vec::new();
        emit_message(&mut buf, std::ptr::null_mut()).expect("to work ;-)");
        assert!(buf.is_empty());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_emit_message() {
        let message = "test message";
        let message_ptr = Box::into_raw(Box::new(message.to_string()));
        let mut buf = Vec::new();
        emit_message(&mut buf, message_ptr).expect("to work ;-)");
        let out = str::from_utf8(&buf).expect("to be valid UTF8");
        assert!(out.contains("BEGIN_MESSAGE"));
        assert!(out.contains("END_MESSAGE"));
        assert!(out.contains(message));
        // Clean up the allocated String
        unsafe { drop(Box::from_raw(message_ptr)) };
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_emit_message_empty_string() {
        let empty_message = String::new();
        let message_ptr = Box::into_raw(Box::new(empty_message));
        let mut buf = Vec::new();

        emit_message(&mut buf, message_ptr).expect("to work");

        // Empty messages should not emit anything
        assert!(buf.is_empty());

        unsafe { drop(Box::from_raw(message_ptr)) };
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_emit_message_whitespace_only() {
        // Whitespace-only messages should not be emitted
        let whitespace_message = "   \n\t  ".to_string();
        let message_ptr = Box::into_raw(Box::new(whitespace_message));
        let mut buf = Vec::new();

        emit_message(&mut buf, message_ptr).expect("to work");

        // Whitespace-only messages should not emit anything
        assert!(buf.is_empty());

        unsafe { drop(Box::from_raw(message_ptr)) };
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_emit_message_with_leading_trailing_whitespace() {
        // Messages with content and whitespace should be emitted (with the whitespace)
        let message_with_whitespace = "  error message  ".to_string();
        let message_ptr = Box::into_raw(Box::new(message_with_whitespace.clone()));
        let mut buf = Vec::new();

        emit_message(&mut buf, message_ptr).expect("to work");
        let out = str::from_utf8(&buf).expect("to be valid UTF8");

        // Should emit markers and preserve whitespace in content
        assert!(out.contains("BEGIN_MESSAGE"));
        assert!(out.contains("END_MESSAGE"));
        assert!(out.contains(&message_with_whitespace));

        unsafe { drop(Box::from_raw(message_ptr)) };
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_emit_message_with_newlines() {
        let message_with_newlines = "line1\nline2\nline3".to_string();
        let message_ptr = Box::into_raw(Box::new(message_with_newlines));
        let mut buf = Vec::new();

        emit_message(&mut buf, message_ptr).expect("to work");
        let out = str::from_utf8(&buf).expect("to be valid UTF8");

        assert!(out.contains("line1"));
        assert!(out.contains("line2"));
        assert!(out.contains("line3"));

        unsafe { drop(Box::from_raw(message_ptr)) };
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_emit_message_unicode() {
        let unicode_message = "Hello 世界 🦀 Rust!".to_string();
        let message_ptr = Box::into_raw(Box::new(unicode_message.clone()));
        let mut buf = Vec::new();

        emit_message(&mut buf, message_ptr).expect("to work");
        let out = str::from_utf8(&buf).expect("to be valid UTF8");

        assert!(out.contains(&unicode_message));

        unsafe { drop(Box::from_raw(message_ptr)) };
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn test_emit_thread_name() {
        let pid = unsafe { libc::getpid() };
        let tid = unsafe { libc::syscall(libc::SYS_gettid) as libc::pid_t };
        let mut buf = Vec::new();

        emit_thread_name(&mut buf, pid, tid).expect("thread name to emit");
        let out = str::from_utf8(&buf).expect("to be valid UTF8");
        assert!(out.contains(DD_CRASHTRACK_BEGIN_THREAD_NAME));
        assert!(out.contains(DD_CRASHTRACK_END_THREAD_NAME));

        let mut comm = String::new();
        let path = format!("/proc/{pid}/task/{tid}/comm");
        File::open(&path)
            .and_then(|mut f| f.read_to_string(&mut comm))
            .expect("read comm");
        let comm_trimmed = comm.trim_end_matches('\n');
        assert!(
            out.contains(comm_trimmed),
            "output should include thread name from comm; got {out:?}"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_emit_message_very_long() {
        let long_message = "x".repeat(100000); // 100KB
        let message_ptr = Box::into_raw(Box::new(long_message.clone()));
        let mut buf = Vec::new();

        emit_message(&mut buf, message_ptr).expect("to work");
        let out = str::from_utf8(&buf).expect("to be valid UTF8");

        assert!(out.contains(&long_message[..100])); // At least first 100 chars

        unsafe { drop(Box::from_raw(message_ptr)) };
    }
}
