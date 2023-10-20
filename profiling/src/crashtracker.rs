use ddcommon::tag::Tag;
use ddcommon::Endpoint;
// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.
use libc::{
    mmap, sigaltstack, MAP_ANON, MAP_FAILED, MAP_PRIVATE, PROT_NONE, PROT_READ, PROT_WRITE,
    SIGSTKSZ,
};
use nix::sys::signal;
use nix::sys::signal::{SaFlags, SigAction, SigHandler};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::prelude::*;
use std::process::{Command, Stdio};
use std::ptr;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::SeqCst;

pub const DD_CRASHTRACK_BEGIN_FILE: &str = "DD_CRASHTRACK_BEGIN_FILE";
pub const DD_CRASHTRACK_BEGIN_SIGINFO: &str = "DD_CRASHTRACK_BEGIN_SIGINFO";
pub const DD_CRASHTRACK_BEGIN_STACKTRACE: &str = "DD_CRASHTRACK_BEGIN_STACKTRACE";
pub const DD_CRASHTRACK_END_FILE: &str = "DD_CRASHTRACK_END_FILE";
pub const DD_CRASHTRACK_END_STACKTRACE: &str = "DD_CRASHTRACK_END_STACKTRACE";
pub const DD_CRASHTRACK_END_SIGINFO: &str = "DD_CRASHTRACK_END_SIGINFO";
pub const DD_CRASHTRACK_BEGIN_COUNTERS: &str = "DD_CRASHTRACK_BEGIN_COUNTERS";
pub const DD_CRASHTRACK_END_COUNTERS: &str = "DD_CRASHTRACK_END_COUNTERS";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    pub profiling_library_name: String,
    pub profiling_library_version: String,
    pub family: String,
    pub tags: Option<Vec<Tag>>,
}

impl Metadata {
    pub fn new(
        profiling_library_name: String,
        profiling_library_version: String,
        family: String,
        tags: Option<Vec<Tag>>,
    ) -> Self {
        Self {
            profiling_library_name,
            profiling_library_version,
            family,
            tags,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Configuration {
    pub endpoint: Endpoint,
    pub path_to_reciever_binary: String,
}

impl Configuration {
    pub fn new(endpoint: Endpoint, path_to_reciever_binary: String) -> Self {
        Self {
            endpoint,
            path_to_reciever_binary,
        }
    }
}

// TODO, add more as needed
/// This is a list of possible operations a profiler might be in, to help us
/// know
/// 1. Whether the profiler was running when the crash happened
/// 2. What it was doing at a broad level
/// This could also be used to track wall clock time, if that's not too expensive
#[repr(C)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ProfilingOpTypes {
    NotProfiling = 0,
    CollectingSample,
    Unwinding,
    Serializing,
    SIZE,
}

impl ProfilingOpTypes {
    pub fn name(i: usize) -> anyhow::Result<&'static str> {
        let rval = match i {
            0 => "not_profiling",
            1 => "collecting_sample",
            2 => "unwinding",
            3 => "serializing",
            _ => anyhow::bail!("invalid enum val {i}"),
        };
        Ok(rval)
    }
}

#[allow(clippy::declare_interior_mutable_const)]
const ATOMIC_ZERO: AtomicUsize = AtomicUsize::new(0);

static NUM_THREADS_DOING_PROFILING: AtomicUsize = ATOMIC_ZERO;
static PROFILING_OP_COUNTERS: [AtomicUsize; ProfilingOpTypes::SIZE as usize] =
    [ATOMIC_ZERO; ProfilingOpTypes::SIZE as usize];

pub fn begin_profiling_op(op: ProfilingOpTypes) -> anyhow::Result<()> {
    NUM_THREADS_DOING_PROFILING.fetch_add(1, SeqCst);
    PROFILING_OP_COUNTERS[op as usize].fetch_add(1, SeqCst);
    // this can technically wrap around, but if we hit 2^64 ops we're doing
    // something else wrong.
    Ok(())
}

pub fn end_profiling_op(op: ProfilingOpTypes) -> anyhow::Result<()> {
    let old = NUM_THREADS_DOING_PROFILING.fetch_sub(1, SeqCst);
    anyhow::ensure!(
        old > 0,
        "attempted to end profiling op '{op:?}' while global count was 0"
    );
    let old = PROFILING_OP_COUNTERS[op as usize].fetch_sub(1, SeqCst);
    anyhow::ensure!(
        old > 0,
        "attempted to end profiling op '{op:?}' while op count was 0"
    );
    Ok(())
}

fn emit_counters(w: &mut impl Write) -> anyhow::Result<()> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_COUNTERS}")?;
    writeln!(
        w,
        "\"num_threads_doing_profiling\": {}",
        NUM_THREADS_DOING_PROFILING.load(SeqCst)
    )?;

    for (i, c) in PROFILING_OP_COUNTERS.iter().enumerate() {
        writeln!(w, "\"{}\": {}", ProfilingOpTypes::name(i)?, c.load(SeqCst))?;
    }

    writeln!(w, "{DD_CRASHTRACK_END_COUNTERS}")?;
    Ok(())
}

static mut RECEIVER: Option<std::process::Child> = None;
static mut OLD_HANDLER: Option<SigAction> = None;
// https://github.com/nix-rust/nix/issues/1051
// On Linux, siginfo_t doesn't really behave right.

// Getting a backtrace on rust is not guaranteed to be signal safe
// https://github.com/rust-lang/backtrace-rs/issues/414
// let current_backtrace = backtrace::Backtrace::new();
// In fact, if we look into the code here, we see mallocs.
// https://doc.rust-lang.org/src/std/backtrace.rs.html#332
fn _emit_backtrace_std(w: &mut impl Write) {
    let current_backtrace = std::backtrace::Backtrace::force_capture();
    writeln!(w, "{:?}", current_backtrace).unwrap();
}

// Getting a backtrace on rust is not guaranteed to be signal safe
// https://github.com/rust-lang/backtrace-rs/issues/414
// My experiemnts show that just calculating the `ip` of the frames seems
// to bo ok for Python, but resolving the frames crashes.
fn emit_backtrace_by_frames(w: &mut impl Write, resolve_frames: bool) -> anyhow::Result<()> {
    // https://docs.rs/backtrace/latest/backtrace/index.html
    writeln!(w, "{DD_CRASHTRACK_BEGIN_STACKTRACE}")?;
    backtrace::trace(|frame| {
        // Write the values we can get without resolving, since these seem to
        // be crash safe in my experiments.
        write! {w, "{{"}.unwrap();
        write!(w, "\"ip\": \"{:?}\", ", frame.ip()).unwrap();
        write!(
            w,
            "\"module_base_address\": \"{:?}\", ",
            frame.module_base_address()
        )
        .unwrap();
        write!(w, "\"sp\": \"{:?}\", ", frame.sp()).unwrap();
        write!(w, "\"symbol_address\": \"{:?}\"", frame.symbol_address()).unwrap();

        if resolve_frames {
            unsafe {
                backtrace::resolve_frame_unsynchronized(frame, |symbol| {
                    if let Some(name) = symbol.name() {
                        writeln!(w, ", name: {}", name).unwrap();
                    }
                    if let Some(filename) = symbol.filename() {
                        writeln!(w, ", filename: {:?}", filename).unwrap();
                    }
                });
            }
        }
        writeln!(w, "}}").unwrap();
        true // keep going to the next frame
    });
    writeln! {w, "{DD_CRASHTRACK_END_STACKTRACE}"}.unwrap();
    Ok(())
}

fn emit_file(w: &mut impl Write, path: &str) -> anyhow::Result<()> {
    let mut file = File::open(path)?;
    const BUFFER_LEN: usize = 512;
    let mut buffer = [0u8; BUFFER_LEN];

    writeln!(w, "{DD_CRASHTRACK_BEGIN_FILE} \"{path}\"")?;

    loop {
        let read_count = file.read(&mut buffer)?;
        w.write_all(&buffer)?;

        if read_count != BUFFER_LEN {
            break;
        }
    }
    writeln!(w, "\n{DD_CRASHTRACK_END_FILE} \"{path}\"")?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn emit_proc_self_maps(w: &mut impl Write) -> anyhow::Result<()> {
    emit_file(w, "/proc/self/maps")?;
    Ok(())
}

fn _handle_sigv_info_impl(
    signum: libc::c_int,
    info: *mut libc::siginfo_t,
    data: *mut libc::c_void, // actually ucontext_t
) -> anyhow::Result<()> {
    let child: &mut std::process::Child = unsafe { RECEIVER.as_mut().unwrap() };
    let pipe = child.stdin.as_mut().unwrap();
    writeln!(pipe, "\"signum\": {signum},")?;
    writeln!(pipe, "\"siginfo\": {:?}", unsafe { *info })?;
    let ucontext = data as *mut libc::ucontext_t;
    writeln!(pipe, "\"ucontext\": {:?}", unsafe { *ucontext })?;

    // Getting a backtrace on rust is not guaranteed to be signal safe
    // https://github.com/rust-lang/backtrace-rs/issues/414
    // let current_backtrace = backtrace::Backtrace::new();
    // In fact, if we look into the code here, we see mallocs.
    // https://doc.rust-lang.org/src/std/backtrace.rs.html#332
    // We could walk the stack ourselves to try to avoid this, but in my
    // experiements doing so with the backtrace crate, we fail in the same
    // cases where the stdlib does.
    emit_backtrace_by_frames(pipe, false)?;
    #[cfg(target_os = "linux")]
    emit_proc_self_maps(pipe)?;

    pipe.flush()?;
    // https://doc.rust-lang.org/std/process/struct.Child.html#method.wait
    // The stdin handle to the child process, if any, will be closed before waiting.
    // This helps avoid deadlock: it ensures that the child does not block waiting
    // for input from the parent, while the parent waits for the child to exit.
    unsafe { RECEIVER.as_mut().unwrap().wait()? };
    Ok(())
}

extern "C" fn _handle_sigsegv_info(
    signum: libc::c_int,
    info: *mut libc::siginfo_t,
    data: *mut libc::c_void, // actually ucontext_t
) {
    // Safety: We've already crashed, this is a best effort to chain to the old
    // behaviour.  Do this first to prevent recursive activation if this handler
    // itself crases (e.g. while calculating stacktrace)
    let _ = unsafe { restore_old_handler() };
    let _ = _handle_sigv_info_impl(signum, info, data);

    // return to old handler (chain).  See comments on `restore_old_handler`.
}

unsafe fn restore_old_handler() -> anyhow::Result<()> {
    // Restore the old handler, so that the current handler can return to it.
    // Although this is technically UB, this is what Rust does in the same case.
    // https://github.com/rust-lang/rust/blob/master/library/std/src/sys/unix/stack_overflow.rs#L75
    let old_handler = unsafe { OLD_HANDLER.as_ref().unwrap() };
    signal::sigaction(signal::SIGSEGV, old_handler)?;
    Ok(())
}

fn handle_segv_impl(signum: i32) -> anyhow::Result<()> {
    let child: &mut std::process::Child = unsafe { RECEIVER.as_mut().unwrap() };
    let pipe = child.stdin.as_mut().unwrap();
    writeln!(pipe, "{DD_CRASHTRACK_BEGIN_SIGINFO}")?;
    writeln!(pipe, "\"signum\": {signum}")?;
    writeln!(pipe, "{DD_CRASHTRACK_END_SIGINFO}")?;

    emit_counters(pipe)?;
    // Getting a backtrace on rust is not guaranteed to be signal safe
    // https://github.com/rust-lang/backtrace-rs/issues/414
    // let current_backtrace = backtrace::Backtrace::new();
    // In fact, if we look into the code here, we see mallocs.
    // https://doc.rust-lang.org/src/std/backtrace.rs.html#332
    // We could walk the stack ourselves to try to avoid this, but in my
    // experiements doing so with the backtrace crate, we fail in the same
    // cases where the stdlib does.
    emit_backtrace_by_frames(pipe, false)?;
    #[cfg(target_os = "linux")]
    emit_proc_self_maps(pipe)?;

    pipe.flush()?;
    // https://doc.rust-lang.org/std/process/struct.Child.html#method.wait
    // The stdin handle to the child process, if any, will be closed before waiting.
    // This helps avoid deadlock: it ensures that the child does not block waiting
    // for input from the parent, while the parent waits for the child to exit.
    unsafe { RECEIVER.as_mut().unwrap().wait()? };
    Ok(())
}

extern "C" fn handle_sigsegv(n: i32) {
    // Safety: We've already crashed, this is a best effort to chain to the old
    // behaviour.  Do this first to prevent recursive activation if this handler
    // itself crases (e.g. while calculating stacktrace)
    let _ = unsafe { restore_old_handler() };
    let _ = handle_segv_impl(n);

    // return to old handler (chain).  See comments on `restore_old_handler`.
}

//TODO, get other signals than segv?
fn register_crash_handler() -> anyhow::Result<()> {
    let sig_action = SigAction::new(
        //SigHandler::SigAction(_handle_sigsegv_info),
        SigHandler::Handler(handle_sigsegv),
        SaFlags::SA_NODEFER | SaFlags::SA_ONSTACK,
        signal::SigSet::empty(),
    );
    unsafe {
        set_alt_stack()?;
        let old = signal::sigaction(signal::SIGSEGV, &sig_action)?;
        OLD_HANDLER = Some(old);
    }
    Ok(())
}

/// Allocates a signal altstack, and puts a guard page at the end.
/// Inspired by https://github.com/rust-lang/rust/pull/69969/files
unsafe fn set_alt_stack() -> anyhow::Result<()> {
    let page_size = page_size::get();
    let stackp = mmap(
        ptr::null_mut(),
        SIGSTKSZ + page_size::get(),
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANON,
        -1,
        0,
    );
    anyhow::ensure!(
        stackp != MAP_FAILED,
        "failed to allocate an alternative stack"
    );
    let guard_result = libc::mprotect(stackp, page_size, PROT_NONE);
    anyhow::ensure!(
        guard_result == 0,
        "failed to set up alternative stack guard page"
    );
    let stackp = stackp.add(page_size);

    let stack = libc::stack_t {
        ss_sp: stackp,
        ss_flags: 0,
        ss_size: SIGSTKSZ,
    };
    let rval = sigaltstack(&stack, ptr::null_mut());
    anyhow::ensure!(rval == 0, "sigaltstack failed {rval}");
    Ok(())
}

//TODO pass key/value pairs to the reciever.
pub fn init(config: Configuration, metadata: Metadata) -> anyhow::Result<()> {
    let receiver = Command::new(&config.path_to_reciever_binary)
        .arg("reciever")
        .stdin(Stdio::piped())
        .spawn()?;

    // Write the args into the reciever.
    // Use the pipe to avoid secrets ending up on the commandline
    writeln!(
        receiver.stdin.as_ref().unwrap(),
        "{}",
        serde_json::to_string(&config)?
    )?;
    writeln!(
        receiver.stdin.as_ref().unwrap(),
        "{}",
        serde_json::to_string(&metadata)?
    )?;
    unsafe {
        RECEIVER = Some(receiver);
    }
    register_crash_handler()?;
    Ok(())
}
