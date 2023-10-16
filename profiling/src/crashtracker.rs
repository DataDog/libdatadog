// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.
use libc::{
    mmap, sigaltstack, MAP_ANON, MAP_FAILED, MAP_PRIVATE, PROT_NONE, PROT_READ, PROT_WRITE,
    SIGSTKSZ,
};
use nix::sys::signal;
use nix::sys::signal::{SaFlags, SigAction, SigHandler};
use std::ffi::OsStr;
use std::fs::File;
use std::io::prelude::*;
use std::process::{Command, Stdio};
use std::ptr;

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
    writeln!(w, "BEGIN backtrace")?;
    backtrace::trace(|frame| {
        let ip = frame.ip();
        writeln!(w, "{ip:?}").unwrap();

        if resolve_frames {
            backtrace::resolve_frame(frame, |symbol| {
                if let Some(name) = symbol.name() {
                    writeln!(w, "name: {}", name).unwrap();
                }
                if let Some(filename) = symbol.filename() {
                    writeln!(w, "filename: {:?}", filename).unwrap();
                }
            });
        }

        true // keep going to the next frame
    });
    writeln!(w, "END backtrace")?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn emit_proc_self_maps(w: &mut impl Write) -> anyhow::Result<()> {
    let mut file = File::open("/proc/self/maps")?;
    const BUFFER_LEN: usize = 512;
    let mut buffer = [0u8; BUFFER_LEN];

    writeln!(w, "BEGIN /proc/self/maps")?;

    loop {
        let read_count = file.read(&mut buffer)?;
        w.write_all(&buffer)?;

        if read_count != BUFFER_LEN {
            break;
        }
    }
    writeln!(w, "\nEND /proc/self/maps")?;
    Ok(())
}

extern "C" fn _handle_sigsegv_info(
    _signum: libc::c_int,
    _info: *mut libc::siginfo_t,
    _data: *mut libc::c_void, // actually ucontext_t
) {
}

unsafe fn restore_old_handler() -> anyhow::Result<()> {
    // Restore the old handler, so that the current handler can return to it.
    // Although this is technically UB, this is what Rust does in the same case.
    // https://github.com/rust-lang/rust/blob/master/library/std/src/sys/unix/stack_overflow.rs#L75
    let old_handler = unsafe { OLD_HANDLER.as_ref().unwrap() };
    signal::sigaction(signal::SIGSEGV, old_handler)?;
    Ok(())
}

fn handle_segv_impl(n: i32) -> anyhow::Result<()> {
    let child: &mut std::process::Child = unsafe { RECEIVER.as_mut().unwrap() };
    let pipe = child.stdin.as_mut().unwrap();
    writeln!(pipe, "Crashed {}", n)?;

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
pub fn init(path_to_reciever_binary: impl AsRef<OsStr>) -> anyhow::Result<()> {
    unsafe {
        RECEIVER = Some(
            Command::new(path_to_reciever_binary)
                .arg("reciever")
                .stdin(Stdio::piped())
                .spawn()?,
        );
    }
    register_crash_handler()?;
    Ok(())
}
