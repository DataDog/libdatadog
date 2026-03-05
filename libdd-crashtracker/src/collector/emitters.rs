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
use crate::{
    translate_si_code, CrashtrackerConfiguration, ErrorKind, SignalNames, StackTrace,
    StacktraceCollection,
};
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
    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),
}

/// Crash-kind-specific data passed to `emit_crashreport`.
///
/// Each variant carries exactly the fields that are meaningful for that crash
/// origin. the shared fields (config, metadata, procinfo, …) remain as plain
/// function parameters
pub(crate) enum CrashKindData {
    UnixSignal {
        sig_info: *const siginfo_t,
        ucontext: *const ucontext_t,
    },
    UnhandledException {
        stacktrace: StackTrace,
    },
}

impl CrashKindData {
    fn error_kind(&self) -> ErrorKind {
        match self {
            CrashKindData::UnixSignal { .. } => ErrorKind::UnixSignal,
            CrashKindData::UnhandledException { .. } => ErrorKind::UnhandledException,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_crashreport(
    pipe: &mut impl Write,
    config: &CrashtrackerConfiguration,
    config_str: &str,
    metadata_string: &str,
    message_ptr: *mut String,
    crash: CrashKindData,
    ppid: i32,
    crashing_tid: libc::pid_t,
) -> Result<(), EmitterError> {
    // Crash-ping
    // The receiver dispatches the crash ping as soon as it sees the metadata
    // section, so try to emit message, siginfo, and kind before it to make sure
    // we have an enhanced crash ping message
    emit_config(pipe, config_str)?;
    emit_message(pipe, message_ptr)?;

    match &crash {
        CrashKindData::UnixSignal { sig_info, .. } => {
            emit_siginfo(pipe, *sig_info)?;
        }
        CrashKindData::UnhandledException { .. } => {
            // Unhandled exceptions have no signal info
        }
    }

    emit_kind(pipe, &crash.error_kind())?;
    emit_metadata(pipe, metadata_string)?;

    // Shared process context
    emit_procinfo(pipe, ppid, crashing_tid)?;
    emit_counters(pipe)?;
    emit_spans(pipe)?;
    consume_and_emit_additional_tags(pipe)?;
    emit_traces(pipe)?;

    #[cfg(target_os = "linux")]
    emit_proc_self_maps(pipe)?;

    // Stack trace emission
    match crash {
        CrashKindData::UnixSignal { ucontext, .. } => {
            emit_ucontext(pipe, ucontext)?;
            if config.resolve_frames() != StacktraceCollection::Disabled {
                // We do this last, so even if it crashes, we still get the other info.
                unsafe { emit_backtrace_by_frames(pipe, config.resolve_frames(), ucontext)? };
            }
            if is_runtime_callback_registered() {
                emit_runtime_stack(pipe)?;
            }
        }
        CrashKindData::UnhandledException { stacktrace } => {
            // SAFETY: this branch only executes when an unhandled exception occurs
            // and is not called from a signal handler.
            unsafe { emit_whole_stacktrace(pipe, stacktrace)? };
        }
    }

    writeln!(pipe, "{DD_CRASHTRACK_DONE}")?;
    pipe.flush()?;
    Ok(())
}

/// Emit a stacktrace onto the given handle as formatted json.
/// SAFETY:
///     Crash-tracking functions are not reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function is not atomic. A crash during its execution may lead to
///     unexpected crash-handling behaviour.
unsafe fn emit_backtrace_by_frames(
    w: &mut impl Write,
    resolve_frames: StacktraceCollection,
    ucontext: *const ucontext_t,
) -> Result<(), EmitterError> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_STACKTRACE}")?;

    // On macOS, backtrace::trace_unsynchronized fails in forked children because
    // macOS restricts many APIs after fork-without-exec. Walk the frame pointer
    // chain directly from the saved ucontext registers instead. The parent's
    // stack memory is still readable in the forked child.
    #[cfg(target_os = "macos")]
    {
        let _ = resolve_frames;
        emit_backtrace_from_ucontext(w, ucontext)?;
    }

    // On Linux, use the bundled libunwind. unw_init_local2(cursor, ucontext, 0)
    // seeds the unwinder from the saved CPU context that the OS captured at the
    // moment of the crash, so we start already past the signal frame at the
    // actual faulting instruction. This is essential on musl libc (Alpine
    // Linux), where the signal trampoline provides no DWARF unwind info and
    // libgcc's unwinder cannot cross the signal frame boundary.
    #[cfg(target_os = "linux")]
    emit_backtrace_via_libunwind(w, resolve_frames, ucontext)?;

    writeln!(w, "{DD_CRASHTRACK_END_STACKTRACE}")?;
    w.flush()?;
    Ok(())
}

/// Unwind the stack using the bundled libunwind, seeded from the OS-captured
/// ucontext.
///
/// `unw_init_local2(cursor, ucontext, 0)` initialises the cursor from the
/// register snapshot that the kernel saved at the moment of the fault. The
/// unwinder therefore starts directly at the faulting instruction; it never
/// has to walk backward through the signal trampoline frame.
///
/// This matters on musl libc (Alpine Linux x86_64): musl's signal trampoline
/// does not carry DWARF unwind info, so libgcc's unwinder (used by the
/// `backtrace` crate) cannot cross the frame boundary and gets stuck inside
/// the signal handler. libunwind's local unwinder has no such limitation.
///
/// For each frame we emit:
///   - `ip`  / `sp`                             — always
///   - `module_base_address` / `symbol_address` — when `dladdr` succeeds
///   - `function`                               — for `EnabledWithInprocessSymbols`
#[cfg(target_os = "linux")]
unsafe fn emit_backtrace_via_libunwind(
    w: &mut impl Write,
    resolve_frames: StacktraceCollection,
    ucontext: *const ucontext_t,
) -> Result<(), EmitterError> {
    use libdd_libunwind_sys::{
        unw_get_proc_name, unw_get_reg, unw_init_local2, unw_step, UnwCursor, UnwWord, UNW_REG_IP,
        UNW_REG_SP,
    };

    if ucontext.is_null() {
        return Ok(());
    }

    let mut cursor: UnwCursor = std::mem::zeroed();
    // Cast away const: libunwind only reads the context to copy the register
    // state into the cursor; it does not modify the ucontext itself.
    let ret = unw_init_local2(&mut cursor, ucontext as *mut _, 0);
    if ret != 0 {
        return Ok(());
    }

    const MAX_FRAMES: usize = 512;
    for _ in 0..MAX_FRAMES {
        let mut ip: UnwWord = 0;
        let mut sp: UnwWord = 0;

        if unw_get_reg(&mut cursor, UNW_REG_IP, &mut ip) != 0 || ip == 0 {
            break;
        }
        let _ = unw_get_reg(&mut cursor, UNW_REG_SP, &mut sp);

        write!(w, "{{\"ip\": \"0x{ip:x}\"")?;
        write!(w, ", \"sp\": \"0x{sp:x}\"")?;

        // dladdr resolves the containing shared object and nearest symbol.
        // It only reads dyld/ld.so internal tables; no allocation, no locks
        let mut dl_info: libc::Dl_info = std::mem::zeroed();
        if libc::dladdr(ip as *const libc::c_void, &mut dl_info) != 0 {
            if !dl_info.dli_fbase.is_null() {
                write!(w, ", \"module_base_address\": \"{:?}\"", dl_info.dli_fbase)?;
            }
            if !dl_info.dli_saddr.is_null() {
                write!(w, ", \"symbol_address\": \"{:?}\"", dl_info.dli_saddr)?;
            }
        }

        if resolve_frames == StacktraceCollection::EnabledWithInprocessSymbols {
            let mut name_buf: [libc::c_char; 256] = [0; 256];
            if unw_get_proc_name(
                &mut cursor,
                name_buf.as_mut_ptr(),
                name_buf.len(),
                std::ptr::null_mut(),
            ) == 0
            {
                let name = std::ffi::CStr::from_ptr(name_buf.as_ptr());
                if let Ok(s) = name.to_str() {
                    write!(w, ", \"function\": \"{s}\"")?;
                }
            }
        }

        writeln!(w, "}}")?;
        // Flush eagerly so each frame is visible even if the next step crashes.
        w.flush()?;

        if unw_step(&mut cursor) <= 0 {
            break;
        }
    }

    Ok(())
}

/// Walk the frame pointer chain from the ucontext's saved registers.
///
/// After fork(), the child process has a copy-on-write view of the parent's
/// stack memory, so the frame pointer chain from the crashed thread is still
/// readable. This avoids depending on `backtrace::trace_unsynchronized` which
/// uses macOS APIs that don't work in forked-but-not-exec'd children.
///
/// For each IP we call `dladdr` to resolve the symbol name, symbol address,
/// and containing shared-object path. `dladdr` is safe here because it only
/// reads dyld's internal data structures (no allocation, no Mach IPC).
#[cfg(target_os = "macos")]
unsafe fn emit_backtrace_from_ucontext(
    w: &mut impl Write,
    ucontext: *const ucontext_t,
) -> Result<(), EmitterError> {
    if ucontext.is_null() {
        return Ok(());
    }
    let mcontext = (*ucontext).uc_mcontext;
    if mcontext.is_null() {
        return Ok(());
    }

    // Get the thread's stack bounds so we only deref frame pointers
    // that lie within known stack memory. Both pthread_get_stackaddr_np and
    // pthread_get_stacksize_np are async-signal-safe on macOS
    let thread = libc::pthread_self();
    let stack_top = libc::pthread_get_stackaddr_np(thread) as usize;
    let stack_size = libc::pthread_get_stacksize_np(thread);
    let stack_bottom = stack_top.saturating_sub(stack_size);

    // Returns true when the range [addr, addr+len) falls within the thread stack
    let in_stack_bounds = |addr: usize, len: usize| -> bool {
        let end = addr.saturating_add(len);
        addr >= stack_bottom && end <= stack_top
    };

    let ss = &(*mcontext).__ss;
    #[cfg(target_arch = "aarch64")]
    let (pc, mut fp) = (ss.__pc as usize, ss.__fp as usize);
    #[cfg(target_arch = "x86_64")]
    let (pc, mut fp) = (ss.__rip as usize, ss.__rbp as usize);

    emit_frame_with_dladdr(w, pc)?;

    const MAX_FRAMES: usize = 512;
    for _ in 0..MAX_FRAMES {
        if fp == 0 || fp % std::mem::align_of::<usize>() != 0 {
            break;
        }
        // Each frame record is two pointer-sized words: [saved_fp, return_addr]
        // Bail out if the record falls outside the thread stack
        if !in_stack_bounds(fp, 2 * std::mem::size_of::<usize>()) {
            break;
        }
        let next_fp = *(fp as *const usize);
        let return_addr = *((fp + std::mem::size_of::<usize>()) as *const usize);
        if return_addr == 0 {
            break;
        }
        emit_frame_with_dladdr(w, return_addr)?;
        if next_fp <= fp {
            break;
        }
        fp = next_fp;
    }

    Ok(())
}

/// Emit a single stack frame, enriched with `dladdr` symbol information.
#[cfg(target_os = "macos")]
unsafe fn emit_frame_with_dladdr(w: &mut impl Write, ip: usize) -> Result<(), EmitterError> {
    let mut info: libc::Dl_info = std::mem::zeroed();
    let resolved = libc::dladdr(ip as *const libc::c_void, &mut info) != 0;

    write!(w, "{{\"ip\": \"0x{ip:x}\"")?;

    if resolved {
        if !info.dli_fbase.is_null() {
            write!(w, ", \"module_base_address\": \"{:?}\"", info.dli_fbase)?;
        }
        if !info.dli_saddr.is_null() {
            write!(w, ", \"symbol_address\": \"{:?}\"", info.dli_saddr)?;
        }
        if !info.dli_sname.is_null() {
            let name = std::ffi::CStr::from_ptr(info.dli_sname);
            if let Ok(s) = name.to_str() {
                write!(w, ", \"function\": \"{s}\"")?;
            }
        }
    }

    writeln!(w, "}}")?;
    w.flush()?;
    Ok(())
}

/// SAFETY:
///    This function is not safe to call from a signal handler.
///    Although `serde_json::to_writer` does not technically allocate memory
///    itself, it takes in `StackTrace` which is allocated and is only intended
///    to be used in a non-signal-handler context
unsafe fn emit_whole_stacktrace(
    w: &mut impl Write,
    stacktrace: StackTrace,
) -> Result<(), EmitterError> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_WHOLE_STACKTRACE}")?;
    let _ = serde_json::to_writer(&mut *w, &stacktrace);
    writeln!(w)?;
    writeln!(w, "{DD_CRASHTRACK_END_WHOLE_STACKTRACE}")?;
    w.flush()?;
    Ok(())
}

fn emit_config(w: &mut impl Write, config_str: &str) -> Result<(), EmitterError> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_CONFIG}")?;
    writeln!(w, "{config_str}")?;
    writeln!(w, "{DD_CRASHTRACK_END_CONFIG}")?;
    w.flush()?;
    Ok(())
}

fn emit_kind<W: std::io::Write>(w: &mut W, kind: &ErrorKind) -> Result<(), EmitterError> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_KIND}")?;
    let _ = serde_json::to_writer(&mut *w, kind);
    writeln!(w)?;
    writeln!(w, "{DD_CRASHTRACK_END_KIND}")?;
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

fn emit_procinfo(w: &mut impl Write, pid: i32, tid: libc::pid_t) -> Result<(), EmitterError> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_PROCINFO}")?;
    writeln!(w, "{{\"pid\": {pid}, \"tid\": {tid} }}")?;
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

#[cfg(test)]
mod tests {
    use crate::StackFrame;

    use super::*;
    use std::str;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_emit_complete_stacktrace() {
        // new_incomplete() starts with incomplete: true, which push_frame requires
        let mut stacktrace = StackTrace::new_incomplete();
        let mut stackframe1 = StackFrame::new();
        stackframe1.with_ip(1234);
        stackframe1.with_function("test_function1".to_string());
        stackframe1.with_file("test_file1".to_string());

        let mut stackframe2 = StackFrame::new();
        stackframe2.with_ip(5678);
        stackframe2.with_function("test_function2".to_string());
        stackframe2.with_file("test_file2".to_string());

        stacktrace.push_frame(stackframe1, true).unwrap();
        stacktrace.push_frame(stackframe2, true).unwrap();

        stacktrace.set_complete().unwrap();

        let mut buf = Vec::new();
        unsafe { emit_whole_stacktrace(&mut buf, stacktrace).expect("to work ;-)") };
        let out = str::from_utf8(&buf).expect("to be valid UTF8");

        assert!(out.contains("\"ip\":\"0x4d2\""));
        assert!(out.contains("\"function\":\"test_function1\""));
        assert!(out.contains("\"file\":\"test_file1\""));
        assert!(out.contains("\"ip\":\"0x162e\""));
        assert!(out.contains("\"function\":\"test_function2\""));
        assert!(out.contains("\"file\":\"test_file2\""));
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
    fn test_emit_procinfo() {
        let pid = unsafe { libc::getpid() };
        let tid = unsafe { libc::syscall(libc::SYS_gettid) as libc::pid_t };
        let mut buf = Vec::new();

        emit_procinfo(&mut buf, pid, tid).expect("procinfo to emit");
        let proc_info_block = str::from_utf8(&buf).expect("to be valid UTF8");
        assert!(proc_info_block.contains(DD_CRASHTRACK_BEGIN_PROCINFO));
        assert!(proc_info_block.contains(DD_CRASHTRACK_END_PROCINFO));

        assert!(proc_info_block.contains(&format!("\"pid\": {pid}")));
        assert!(proc_info_block.contains(&format!("\"tid\": {tid}")));
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

    // We only test edge cases specific to this wrapper function here.
    // The core unwinding logic is tested in the libunwind crate.
    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn test_emit_backtrace_via_libunwind_null_ucontext() {
        let mut buf = Vec::new();
        unsafe {
            emit_backtrace_via_libunwind(
                &mut buf,
                StacktraceCollection::WithoutSymbols,
                std::ptr::null(),
            )
            .expect("should handle null ucontext gracefully");
        }
        // With null ucontext, function should return early and emit nothing
        assert!(buf.is_empty());
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn test_emit_backtrace_via_libunwind_unw_init_failure() {
        // Test that when unw_init_local2 fails (e.g., with invalid context),
        // the function returns Ok(()) gracefully without writing anything
        let context: libc::ucontext_t = unsafe { std::mem::zeroed() };
        let mut buf = Vec::new();

        unsafe {
            emit_backtrace_via_libunwind(&mut buf, StacktraceCollection::WithoutSymbols, &context)
                .expect("should handle unw_init_local2 failure gracefully");
        }

        // When unw_init_local2 fails, function should return early without error
        // Buffer should be empty since no frames were written
        assert!(
            buf.is_empty(),
            "Function should return early on unw_init_local2 failure"
        );
    }
}
