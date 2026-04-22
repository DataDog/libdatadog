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

#[cfg(target_os = "linux")]
use std::io::BufRead;

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
                // SAFETY: `ucontext` comes from the signal handler and points to
                // valid kernel-saved registers. This is called last so that even
                // if the unwinder crashes, the other crash data has already been
                // written. The crash handler is non-reentrant and single-threaded
                unsafe { emit_backtrace_by_frames(pipe, config.resolve_frames(), ucontext)? };
            }
            if is_runtime_callback_registered() {
                emit_runtime_stack(pipe)?;
            }
        }
        CrashKindData::UnhandledException { stacktrace } => {
            // SAFETY: This branch only executes for unhandled exceptions, never
            // from a signal handler
            unsafe { emit_whole_stacktrace(pipe, stacktrace)? };
        }
    }

    // Emit other threads (Phase 1: name/state; Phase 2: also stack if context available).
    #[cfg(target_os = "linux")]
    if config.collect_all_threads() {
        let _ = emit_all_threads(pipe, config, ppid, crashing_tid);
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
        // SAFETY: `ucontext` originates from the signal handler and points to
        // the kernel-saved register snapshot. The caller guarantees we are in a
        // crash-handling context where the parent's stack is still readable
        // (copy-on-write after fork)
        unsafe { emit_macos_backtrace_from_ucontext(w, ucontext)? };
    }

    // On Linux, use the bundled libunwind. unw_init_local2(cursor, ucontext, 0)
    // seeds the unwinder from the saved CPU context that the OS captured at the
    // moment of the crash, so we start already past the signal frame at the
    // actual faulting instruction. This is essential on musl libc (Alpine
    // Linux), where the signal trampoline provides no DWARF unwind info and
    // libgcc's unwinder cannot cross the signal frame boundary.
    #[cfg(target_os = "linux")]
    // SAFETY: `ucontext` originates from the signal handler and points to the
    // kernel-saved register snapshot. The caller guarantees single-threaded,
    // non-reentrant crash-handler execution
    unsafe {
        emit_backtrace_via_libunwind(w, resolve_frames, ucontext)?
    };
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
/// We choose to use step instead of backtrace2, because we want to eagerly flush
/// frame by frame
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
        unw_get_proc_name, unw_get_reg, unw_init_local2, unw_step, UnwCursor, UnwWord, UNW_REG_FP,
        UNW_REG_IP, UNW_REG_SP,
    };

    if ucontext.is_null() {
        return Ok(());
    }

    // SAFETY: UnwCursor is a repr(C) struct of plain integers (`[u64; 127]`);
    // all-zeros is a valid bit pattern
    let mut cursor: UnwCursor = unsafe { std::mem::zeroed() };

    // SAFETY: `cursor` is zeroed and is valid for initialization.
    // `ucontext` was checked non-null above and points to the kernel-saved
    // register snapshot captured by the signal handler. The const-to-mut cast
    // is ok: libunwind only reads the context to seed the cursor
    let ret = unsafe { unw_init_local2(&mut cursor, ucontext as *mut _, 1) };
    if ret != 0 {
        return Ok(());
    }

    const MAX_FRAMES: usize = 512;
    for _ in 0..MAX_FRAMES {
        let mut ip: UnwWord = 0;
        let mut sp: UnwWord = 0;
        let mut fp: UnwWord = 0;

        // SAFETY: `cursor` was successfully initialized by `unw_init_local2`
        // and is advanced by `unw_step` at the end of each iteration.
        // UNW_REG_IP and UNW_REG_SP are valid libunwind register constants
        if unsafe { unw_get_reg(&mut cursor, UNW_REG_IP, &mut ip) } != 0 || ip == 0 {
            break;
        }
        let _ = unsafe { unw_get_reg(&mut cursor, UNW_REG_SP, &mut sp) };
        let _ = unsafe { unw_get_reg(&mut cursor, UNW_REG_FP, &mut fp) };

        write!(w, "{{\"ip\": \"0x{ip:x}\"")?;
        write!(w, ", \"sp\": \"0x{sp:x}\"")?;
        write!(w, ", \"fp\": \"0x{fp:x}\"")?;

        // SAFETY: Dl_info is a repr(C) struct of pointers and integers;
        // all-zeros (null pointers, zero integers) is a valid representation
        let mut dl_info: libc::Dl_info = unsafe { std::mem::zeroed() };
        // SAFETY: `ip` is a code address obtained from the unwinder.
        // dladdr only reads ld.so internal tables (no allocation, no locks)
        // making it safe to call from a signal handler
        if unsafe { libc::dladdr(ip as *const libc::c_void, &mut dl_info) } != 0 {
            if !dl_info.dli_fbase.is_null() {
                write!(w, ", \"module_base_address\": \"{:?}\"", dl_info.dli_fbase)?;
            }
            if !dl_info.dli_saddr.is_null() {
                write!(w, ", \"symbol_address\": \"{:?}\"", dl_info.dli_saddr)?;
            }
        }

        if resolve_frames == StacktraceCollection::EnabledWithInprocessSymbols {
            let mut name_buf: [libc::c_char; 256] = [0; 256];
            // SAFETY: `cursor` is in a valid state (unw_get_reg succeeded).
            // `name_buf` is a valid stack-allocated buffer with known length.
            if unsafe {
                unw_get_proc_name(
                    &mut cursor,
                    name_buf.as_mut_ptr(),
                    name_buf.len(),
                    std::ptr::null_mut(),
                )
            } == 0
            {
                // SAFETY: unw_get_proc_name returned 0 (success), guaranteeing
                // a NUL-terminated string was written into name_buf.
                let name = unsafe { std::ffi::CStr::from_ptr(name_buf.as_ptr()) };
                if let Ok(s) = name.to_str() {
                    write!(w, ", \"function\": \"{s}\"")?;
                }
            }
        }

        writeln!(w, "}}")?;
        w.flush()?;

        // SAFETY: `cursor` is in a valid state; unw_step advances to the next
        // frame or returns <= 0 when no more frames remain.
        if unsafe { unw_step(&mut cursor) } <= 0 {
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
unsafe fn emit_macos_backtrace_from_ucontext(
    w: &mut impl Write,
    ucontext: *const ucontext_t,
) -> Result<(), EmitterError> {
    if ucontext.is_null() {
        return Ok(());
    }
    let mcontext = unsafe { (*ucontext).uc_mcontext };
    if mcontext.is_null() {
        return Ok(());
    }

    // SAFETY: pthread_self and pthread_get_stack{addr,size}_np are
    // async-signal-safe on macOS and always succeed for the calling thread.
    let thread = unsafe { libc::pthread_self() };
    let stack_top = unsafe { libc::pthread_get_stackaddr_np(thread) } as usize;
    let stack_size = unsafe { libc::pthread_get_stacksize_np(thread) };
    let stack_bottom = stack_top.saturating_sub(stack_size);

    let in_stack_bounds = |addr: usize, len: usize| -> bool {
        let end = addr.saturating_add(len);
        addr >= stack_bottom && end <= stack_top
    };

    // SAFETY: `mcontext` was checked non-null above and is the kernel-provided
    // machine context from the signal handler's ucontext.
    let ss = unsafe { &(*mcontext).__ss };
    #[cfg(target_arch = "aarch64")]
    let (pc, mut fp) = (ss.__pc as usize, ss.__fp as usize);
    #[cfg(target_arch = "x86_64")]
    let (pc, mut fp) = (ss.__rip as usize, ss.__rbp as usize);

    // SAFETY: `pc` is a valid code address from the kernel-saved register state.
    unsafe { emit_frame_with_dladdr(w, pc)? };

    const MAX_FRAMES: usize = 512;
    for _ in 0..MAX_FRAMES {
        if fp == 0 || fp % std::mem::align_of::<usize>() != 0 {
            break;
        }
        if !in_stack_bounds(fp, 2 * std::mem::size_of::<usize>()) {
            break;
        }
        // SAFETY: `fp` is non-zero, properly aligned, and the two-word frame
        // record [saved_fp, return_addr] lies within the validated thread stack
        // bounds (checked by in_stack_bounds above). After fork(), the child
        // has a copy-on-write view of the parent's stack memory.
        let next_fp = unsafe { *(fp as *const usize) };
        let return_addr = unsafe { *((fp + std::mem::size_of::<usize>()) as *const usize) };
        if return_addr == 0 {
            break;
        }
        // SAFETY: `return_addr` is a code address read from a validated
        // in-bounds frame record on the thread stack.
        unsafe { emit_frame_with_dladdr(w, return_addr)? };
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
    // SAFETY: Dl_info is a repr(C) struct of pointers and integers;
    // all-zeros (null pointers, zero integers) is a valid representation.
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    // SAFETY: dladdr only reads dyld's internal data structures (no
    // allocation, no Mach IPC) making it async-signal-safe. `ip` is a code
    // address from the unwound stack or kernel-saved registers.
    let resolved = unsafe { libc::dladdr(ip as *const libc::c_void, &mut info) } != 0;

    write!(w, "{{\"ip\": \"0x{ip:x}\"")?;

    if resolved {
        if !info.dli_fbase.is_null() {
            write!(w, ", \"module_base_address\": \"{:?}\"", info.dli_fbase)?;
        }
        if !info.dli_saddr.is_null() {
            write!(w, ", \"symbol_address\": \"{:?}\"", info.dli_saddr)?;
        }
        if !info.dli_sname.is_null() {
            // SAFETY: dladdr returned non-zero and dli_sname is non-null, so
            // it points to a valid NUL-terminated C string in the shared
            // library's string table (static lifetime, read-only).
            let name = unsafe { std::ffi::CStr::from_ptr(info.dli_sname) };
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

/// Emit one thread block over the wire protocol.
///
/// Format:
/// ```text
/// DD_CRASHTRACK_BEGIN_THREAD
/// {"tid": <tid>, "crashed": <bool>, "name": "<name>", "state": "<state>"}
/// DD_CRASHTRACK_BEGIN_STACKTRACE
/// ...frame JSON lines (if a ucontext was captured for this thread)...
/// DD_CRASHTRACK_END_STACKTRACE
/// DD_CRASHTRACK_END_THREAD
/// ```
#[cfg(target_os = "linux")]
fn emit_thread_block(
    w: &mut impl Write,
    tid: libc::pid_t,
    crashed: bool,
    name: &str,
    state: Option<&str>,
    resolve_frames: StacktraceCollection,
    ucontext: Option<*const ucontext_t>,
) -> Result<(), EmitterError> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_THREAD}")?;

    // Header JSON line
    write!(w, "{{\"tid\": {tid}, \"crashed\": {crashed}")?;
    let safe_name = name
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    write!(w, ", \"name\": \"{safe_name}\"")?;
    if let Some(s) = state {
        let safe_state = s
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        write!(w, ", \"state\": \"{safe_state}\"")?;
    }
    writeln!(w, "}}")?;
    w.flush()?;

    // Stack trace (only if we have a ucontext for this thread)
    writeln!(w, "{DD_CRASHTRACK_BEGIN_STACKTRACE}")?;
    if let Some(uc) = ucontext {
        if resolve_frames != StacktraceCollection::Disabled {
            // SAFETY: uc was written by handle_collect_context_signal and is valid for
            // the duration of the collector child (COW mapping from the parent).
            let _ = unsafe { emit_backtrace_via_libunwind(w, resolve_frames, uc) };
        }
    }
    writeln!(w, "{DD_CRASHTRACK_END_STACKTRACE}")?;
    w.flush()?;

    writeln!(w, "{DD_CRASHTRACK_END_THREAD}")?;
    w.flush()?;
    Ok(())
}

/// Read a thread's name from /proc/<pid>/task/<tid>/comm.
#[cfg(target_os = "linux")]
fn read_thread_name(pid: libc::pid_t, tid: libc::pid_t) -> Option<String> {
    let path = format!("/proc/{pid}/task/{tid}/comm");
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim_end_matches('\n').to_string())
}

/// Read a thread's scheduler state letter from /proc/<pid>/task/<tid>/status.
/// Returns the single-letter state ("S", "R", "D") or None on failure.
#[cfg(target_os = "linux")]
fn read_thread_state(pid: libc::pid_t, tid: libc::pid_t) -> Option<String> {
    let path = format!("/proc/{pid}/task/{tid}/status");
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    for line in reader.lines() {
        let line = line.ok()?;
        if let Some(rest) = line.strip_prefix("State:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Enumerate all live thread IDs under /proc/<pid>/task/ using std::fs (safe to
/// call from the collector child, where there are no async-signal-safety constraints).
#[cfg(target_os = "linux")]
fn enumerate_task_tids(pid: libc::pid_t) -> Vec<libc::pid_t> {
    let path = format!("/proc/{pid}/task");
    let Ok(entries) = std::fs::read_dir(&path) else {
        return vec![];
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .and_then(|s| s.parse::<libc::pid_t>().ok())
        })
        .collect()
}

/// Emit thread blocks for all threads other than the crashing thread.
///
/// Called from the collector child (after fork), so std::fs and ptrace are safe to use.
/// Uses a streaming approach to avoid allocating vectors or hashmaps.
/// For each thread:
///   - Uses ptrace to capture thread context (registers + stack)
///   - Reads name and state from /proc/<ppid>/task/<tid>/
///   - Immediately emits the thread block without intermediate storage
#[cfg(target_os = "linux")]
fn emit_all_threads(
    w: &mut impl Write,
    config: &CrashtrackerConfiguration,
    ppid: libc::pid_t,
    crashing_tid: libc::pid_t,
) -> Result<(), EmitterError> {
    use crate::collector::ptrace_collector::stream_thread_contexts;
    use std::time::Duration;

    // Calculate timeout for ptrace operations
    let context_timeout = Duration::from_millis((config.timeout().as_millis() / 2).min(200) as u64);

    let result = stream_thread_contexts(
        ppid,
        crashing_tid,
        config.max_threads(),
        context_timeout,
        |tid, captured_context| {
            // Read thread metadata from /proc
            let name = read_thread_name(ppid, tid).unwrap_or_else(|| tid.to_string());
            let state = read_thread_state(ppid, tid);

            // Get ucontext pointer if we captured context for this thread
            let ucontext = captured_context.map(|ctx| &ctx.ucontext as *const _);

            // Immediately emit the thread block
            match emit_thread_block(
                w,
                tid,
                false,
                &name,
                state.as_deref(),
                config.resolve_frames(),
                ucontext,
            ) {
                Ok(()) => true,  // Continue with next thread
                Err(_) => false, // Stop iteration on write error
            }
        },
    );

    // Handle the case where ptrace setup fails entirely
    if result.is_err() {
        // Fall back to thread enumeration without context capture
        // This provides basic thread information even when ptrace fails
        let tids = enumerate_task_tids(ppid);
        let max = config.max_threads();
        let mut emitted = 0;

        for tid in tids {
            if tid == crashing_tid {
                continue;
            }
            if emitted >= max {
                break;
            }

            let name = read_thread_name(ppid, tid).unwrap_or_else(|| tid.to_string());
            let state = read_thread_state(ppid, tid);

            emit_thread_block(
                w,
                tid,
                false,
                &name,
                state.as_deref(),
                config.resolve_frames(),
                None, // No context available
            )?;
            emitted += 1;
        }
    }

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
    let uc = unsafe { &*ucontext };

    #[cfg(target_arch = "x86_64")]
    {
        let gregs = &uc.uc_mcontext.gregs;
        write!(w, "{{\"arch\": \"x86_64\", \"registers\": {{")?;
        write!(w, "\"rip\": \"0x{:016x}\"", gregs[libc::REG_RIP as usize])?;
        write!(w, ", \"rsp\": \"0x{:016x}\"", gregs[libc::REG_RSP as usize])?;
        write!(w, ", \"rbp\": \"0x{:016x}\"", gregs[libc::REG_RBP as usize])?;
        write!(w, ", \"rax\": \"0x{:016x}\"", gregs[libc::REG_RAX as usize])?;
        write!(w, ", \"rbx\": \"0x{:016x}\"", gregs[libc::REG_RBX as usize])?;
        write!(w, ", \"rcx\": \"0x{:016x}\"", gregs[libc::REG_RCX as usize])?;
        write!(w, ", \"rdx\": \"0x{:016x}\"", gregs[libc::REG_RDX as usize])?;
        write!(w, ", \"rsi\": \"0x{:016x}\"", gregs[libc::REG_RSI as usize])?;
        write!(w, ", \"rdi\": \"0x{:016x}\"", gregs[libc::REG_RDI as usize])?;
        write!(w, ", \"r8\": \"0x{:016x}\"", gregs[libc::REG_R8 as usize])?;
        write!(w, ", \"r9\": \"0x{:016x}\"", gregs[libc::REG_R9 as usize])?;
        write!(w, ", \"r10\": \"0x{:016x}\"", gregs[libc::REG_R10 as usize])?;
        write!(w, ", \"r11\": \"0x{:016x}\"", gregs[libc::REG_R11 as usize])?;
        write!(w, ", \"r12\": \"0x{:016x}\"", gregs[libc::REG_R12 as usize])?;
        write!(w, ", \"r13\": \"0x{:016x}\"", gregs[libc::REG_R13 as usize])?;
        write!(w, ", \"r14\": \"0x{:016x}\"", gregs[libc::REG_R14 as usize])?;
        write!(w, ", \"r15\": \"0x{:016x}\"", gregs[libc::REG_R15 as usize])?;
        // Preserve the full ucontext as a raw Debug string so that FPU state,
        // signal mask, and alternate-stack info are not lost.
        write!(w, "}}, \"raw\": \"{:?}\"", uc)?;
        writeln!(w, "}}")?;
    }

    #[cfg(target_arch = "aarch64")]
    {
        let mc = &uc.uc_mcontext;
        write!(w, "{{\"arch\": \"aarch64\", \"registers\": {{")?;
        write!(w, "\"pc\": \"0x{:016x}\"", mc.pc)?;
        write!(w, ", \"sp\": \"0x{:016x}\"", mc.sp)?;
        for i in 0..31 {
            write!(w, ", \"x{}\": \"0x{:016x}\"", i, mc.regs[i])?;
        }
        write!(w, "}}, \"raw\": \"{:?}\"", uc)?;
        writeln!(w, "}}")?;
    }

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
    // SAFETY: Reads from a global atomic pointer set during crashtracker
    // initialization. The crash handler's non-reentrant execution model
    // guarantees no concurrent modification.
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
    // SAFETY: The runtime callback was registered during initialization and
    // must be signal-safe per its API contract. The crash handler's
    // non-reentrant model ensures no concurrent invocation.
    unsafe { invoke_runtime_callback_with_writer(w)? };
    writeln!(w, "{DD_CRASHTRACK_END_RUNTIME_STACK_FRAME}")?;
    w.flush()?;
    Ok(())
}

fn emit_runtime_stack_by_stacktrace_string(w: &mut impl Write) -> Result<(), EmitterError> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_RUNTIME_STACK_STRING}")?;
    // SAFETY: Same contract as emit_runtime_stack_by_frames — the callback
    // was registered at init time and the crash handler runs non-reentrantly.
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
    let uc = unsafe { &*ucontext };
    let mcontext = uc.uc_mcontext;
    writeln!(w, "{DD_CRASHTRACK_BEGIN_UCONTEXT}")?;

    if mcontext.is_null() {
        // Fall back to raw Debug output if mcontext pointer is null.
        write!(w, "{{\"arch\": \"")?;
        #[cfg(target_arch = "x86_64")]
        write!(w, "x86_64")?;
        #[cfg(target_arch = "aarch64")]
        write!(w, "aarch64")?;
        write!(w, "\", \"registers\": {{}}")?;
        write!(w, ", \"raw\": \"{:?}\"", uc)?;
        writeln!(w, "}}")?;
    } else {
        // SAFETY: mcontext is non-null, provided by the signal handler.
        let mc = unsafe { &*mcontext };
        let ss = &mc.__ss;

        #[cfg(target_arch = "x86_64")]
        {
            write!(w, "{{\"arch\": \"x86_64\", \"registers\": {{")?;
            write!(w, "\"rip\": \"0x{:016x}\"", ss.__rip)?;
            write!(w, ", \"rsp\": \"0x{:016x}\"", ss.__rsp)?;
            write!(w, ", \"rbp\": \"0x{:016x}\"", ss.__rbp)?;
            write!(w, ", \"rax\": \"0x{:016x}\"", ss.__rax)?;
            write!(w, ", \"rbx\": \"0x{:016x}\"", ss.__rbx)?;
            write!(w, ", \"rcx\": \"0x{:016x}\"", ss.__rcx)?;
            write!(w, ", \"rdx\": \"0x{:016x}\"", ss.__rdx)?;
            write!(w, ", \"rsi\": \"0x{:016x}\"", ss.__rsi)?;
            write!(w, ", \"rdi\": \"0x{:016x}\"", ss.__rdi)?;
            write!(w, ", \"r8\": \"0x{:016x}\"", ss.__r8)?;
            write!(w, ", \"r9\": \"0x{:016x}\"", ss.__r9)?;
            write!(w, ", \"r10\": \"0x{:016x}\"", ss.__r10)?;
            write!(w, ", \"r11\": \"0x{:016x}\"", ss.__r11)?;
            write!(w, ", \"r12\": \"0x{:016x}\"", ss.__r12)?;
            write!(w, ", \"r13\": \"0x{:016x}\"", ss.__r13)?;
            write!(w, ", \"r14\": \"0x{:016x}\"", ss.__r14)?;
            write!(w, ", \"r15\": \"0x{:016x}\"", ss.__r15)?;
            write!(w, "}}, \"raw\": \"{:?}, {:?}\"", uc, mc)?;
            writeln!(w, "}}")?;
        }

        #[cfg(target_arch = "aarch64")]
        {
            write!(w, "{{\"arch\": \"aarch64\", \"registers\": {{")?;
            write!(w, "\"pc\": \"0x{:016x}\"", ss.__pc)?;
            write!(w, ", \"sp\": \"0x{:016x}\"", ss.__sp)?;
            write!(w, ", \"fp\": \"0x{:016x}\"", ss.__fp)?;
            write!(w, ", \"lr\": \"0x{:016x}\"", ss.__lr)?;
            for i in 0..29 {
                write!(w, ", \"x{}\": \"0x{:016x}\"", i, ss.__x[i])?;
            }
            write!(w, "}}, \"raw\": \"{:?}, {:?}\"", uc, mc)?;
            writeln!(w, "}}")?;
        }
    }

    writeln!(w, "{DD_CRASHTRACK_END_UCONTEXT}")?;
    w.flush()?;
    Ok(())
}

fn emit_siginfo(w: &mut impl Write, sig_info: *const siginfo_t) -> Result<(), EmitterError> {
    if sig_info.is_null() {
        return Err(EmitterError::NullSiginfo);
    }

    // SAFETY: `sig_info` was checked non-null above and points to the
    // kernel-provided siginfo_t from the signal handler.
    let si_signo = unsafe { (*sig_info).si_signo };
    let si_signo_human_readable: SignalNames = si_signo.into();

    // Derive the faulting address from `sig_info`
    // https://man7.org/linux/man-pages/man2/sigaction.2.html
    // SIGILL, SIGFPE, SIGSEGV, SIGBUS, and SIGTRAP fill in si_addr with the address of the fault.
    let si_addr: Option<usize> = match si_signo {
        libc::SIGILL | libc::SIGFPE | libc::SIGSEGV | libc::SIGBUS | libc::SIGTRAP => {
            // SAFETY: for these signal types, si_addr is defined and valid
            // per sigaction(2). `sig_info` was checked non-null above.
            Some(unsafe { (*sig_info).si_addr() as usize })
        }
        _ => None,
    };

    // SAFETY: `sig_info` was checked non-null and points to valid kernel data.
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

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_emit_ucontext_null_pointer() {
        let mut buf = Vec::new();
        let result = emit_ucontext(&mut buf, std::ptr::null());

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), EmitterError::NullUcontext));
        assert!(buf.is_empty());
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn test_emit_ucontext_linux_valid() {
        // Create a minimal valid ucontext_t with zeroed register values
        let mut context: libc::ucontext_t = unsafe { std::mem::zeroed() };

        // Set up some test register values
        #[cfg(target_arch = "x86_64")]
        {
            // Set some register values for testing
            context.uc_mcontext.gregs[libc::REG_RIP as usize] = 0x12345678;
            context.uc_mcontext.gregs[libc::REG_RSP as usize] = 0x87654321;
            context.uc_mcontext.gregs[libc::REG_RBP as usize] = 0xABCDEF00;
        }

        #[cfg(target_arch = "aarch64")]
        {
            // Set some register values for testing
            context.uc_mcontext.pc = 0x12345678;
            context.uc_mcontext.sp = 0x87654321;
            context.uc_mcontext.regs[0] = 0xABCDEF00;
        }

        let mut buf = Vec::new();
        emit_ucontext(&mut buf, &context).expect("emit_ucontext should succeed");

        let output = str::from_utf8(&buf).expect("output should be valid UTF-8");

        // Check that proper markers are present
        assert!(output.contains(crate::shared::constants::DD_CRASHTRACK_BEGIN_UCONTEXT));
        assert!(output.contains(crate::shared::constants::DD_CRASHTRACK_END_UCONTEXT));

        // Check architecture is correct
        #[cfg(target_arch = "x86_64")]
        {
            assert!(output.contains("\"arch\": \"x86_64\""));
            assert!(output.contains("\"registers\""));

            // Check specific registers are present
            assert!(output.contains("\"rip\""));
            assert!(output.contains("\"rsp\""));
            assert!(output.contains("\"rbp\""));
            assert!(output.contains("\"rax\""));

            // Check our test values are formatted correctly
            assert!(output.contains("0x0000000012345678")); // rip
            assert!(output.contains("0x0000000087654321")); // rsp
            assert!(output.contains("0x00000000abcdef00")); // rbp
        }

        #[cfg(target_arch = "aarch64")]
        {
            assert!(output.contains("\"arch\": \"aarch64\""));
            assert!(output.contains("\"registers\""));

            // Check specific registers are present
            assert!(output.contains("\"pc\""));
            assert!(output.contains("\"sp\""));
            assert!(output.contains("\"x0\""));

            // Check our test values are formatted correctly
            assert!(output.contains("0x0000000012345678")); // pc
            assert!(output.contains("0x0000000087654321")); // sp
            assert!(output.contains("0x00000000abcdef00")); // x0
        }

        // Check that raw debug output is included
        assert!(output.contains("\"raw\""));

        // Verify it's valid JSON between the markers
        let start_marker = crate::shared::constants::DD_CRASHTRACK_BEGIN_UCONTEXT;
        let end_marker = crate::shared::constants::DD_CRASHTRACK_END_UCONTEXT;

        let start_pos = output.find(start_marker).unwrap() + start_marker.len() + 1; // +1 for newline
        let end_pos = output.find(end_marker).unwrap();
        let json_part = output[start_pos..end_pos].trim();

        let parsed: serde_json::Value =
            serde_json::from_str(json_part).expect("JSON between markers should be valid");

        // Verify the JSON structure
        assert!(parsed.is_object());
        assert!(parsed["arch"].is_string());
        assert!(parsed["registers"].is_object());
        assert!(parsed["raw"].is_string());
    }

    #[test]
    #[cfg(target_os = "macos")]
    #[cfg_attr(miri, ignore)]
    fn test_emit_ucontext_macos_valid() {
        use libc::__darwin_ucontext;
        // Create a minimal valid ucontext_t for macOS
        let mut context: __darwin_ucontext = unsafe { std::mem::zeroed() };

        // On macOS, we need to allocate mcontext and set up the pointer
        let mut mcontext: libc::__darwin_mcontext64 = unsafe { std::mem::zeroed() };
        context.uc_mcontext = &mut mcontext as *mut libc::__darwin_mcontext64;

        // Set up some test register values
        #[cfg(target_arch = "x86_64")]
        {
            unsafe {
                (*context.uc_mcontext).__ss.__rip = 0x12345678;
                (*context.uc_mcontext).__ss.__rsp = 0x87654321;
                (*context.uc_mcontext).__ss.__rbp = 0xABCDEF00;
            }
        }

        #[cfg(target_arch = "aarch64")]
        {
            unsafe {
                (*context.uc_mcontext).__ss.__pc = 0x12345678;
                (*context.uc_mcontext).__ss.__sp = 0x87654321;
                (*context.uc_mcontext).__ss.__fp = 0xABCDEF00;
            }
        }

        let mut buf = Vec::new();
        emit_ucontext(&mut buf, &context as *const __darwin_ucontext)
            .expect("emit_ucontext should succeed");

        let output = str::from_utf8(&buf).expect("output should be valid UTF-8");

        // Check that proper markers are present
        assert!(output.contains(crate::shared::constants::DD_CRASHTRACK_BEGIN_UCONTEXT));
        assert!(output.contains(crate::shared::constants::DD_CRASHTRACK_END_UCONTEXT));

        // Check architecture is correct
        #[cfg(target_arch = "x86_64")]
        {
            assert!(output.contains("\"arch\": \"x86_64\""));
            assert!(output.contains("\"registers\""));

            // Check specific registers are present
            assert!(output.contains("\"rip\""));
            assert!(output.contains("\"rsp\""));
            assert!(output.contains("\"rbp\""));

            // Check our test values are formatted correctly
            assert!(output.contains("0x0000000012345678")); // rip
            assert!(output.contains("0x0000000087654321")); // rsp
            assert!(output.contains("0x00000000abcdef00")); // rbp
        }

        #[cfg(target_arch = "aarch64")]
        {
            assert!(output.contains("\"arch\": \"aarch64\""));
            assert!(output.contains("\"registers\""));

            // Check specific registers are present
            assert!(output.contains("\"pc\""));
            assert!(output.contains("\"sp\""));
            assert!(output.contains("\"fp\""));

            // Check our test values are formatted correctly
            assert!(output.contains("0x0000000012345678")); // pc
            assert!(output.contains("0x0000000087654321")); // sp
            assert!(output.contains("0x00000000abcdef00")); // fp
        }

        // Check that raw debug output is included
        assert!(output.contains("\"raw\""));
    }

    #[test]
    #[cfg(target_os = "macos")]
    #[cfg_attr(miri, ignore)]
    fn test_emit_ucontext_macos_null_mcontext() {
        // Test the fallback case when mcontext is null
        let mut context: libc::ucontext_t = unsafe { std::mem::zeroed() };
        context.uc_mcontext = std::ptr::null_mut(); // Explicitly set to null

        let mut buf = Vec::new();
        emit_ucontext(&mut buf, &context).expect("emit_ucontext should succeed with null mcontext");

        let output = str::from_utf8(&buf).expect("output should be valid UTF-8");

        // Check that proper markers are present
        assert!(output.contains(crate::shared::constants::DD_CRASHTRACK_BEGIN_UCONTEXT));
        assert!(output.contains(crate::shared::constants::DD_CRASHTRACK_END_UCONTEXT));

        // Should contain fallback information
        assert!(output.contains("\"registers\": {}"));
        assert!(output.contains("\"raw\""));

        #[cfg(target_arch = "x86_64")]
        assert!(output.contains("\"arch\": \"x86_64\""));

        #[cfg(target_arch = "aarch64")]
        assert!(output.contains("\"arch\": \"aarch64\""));
    }
}
