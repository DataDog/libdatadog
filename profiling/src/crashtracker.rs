// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.
use libc::{
    mmap, sigaltstack, MAP_ANON, MAP_FAILED, MAP_PRIVATE, PROT_NONE, PROT_READ, PROT_WRITE,
    SIGSTKSZ,
};
use nix::sys::signal;
use nix::sys::signal::{SaFlags, SigAction, SigHandler};
use std::ffi::OsStr;
use std::io::prelude::*;
use std::process::{Command, Stdio};
use std::ptr;

static mut RECEIVER: Option<std::process::Child> = None;

extern "C" fn handle_sigsegv(n: i32) {
    let child = unsafe { RECEIVER.as_mut().unwrap() };
    let pipe = child.stdin.as_mut().unwrap();
    writeln!(pipe, "Crashed {}", n).unwrap();
    //let current_backtrace = backtrace::Backtrace::new();
    let current_backtrace = std::backtrace::Backtrace::force_capture();
    writeln!(pipe, "{:?}", current_backtrace).unwrap();
    pipe.flush().unwrap();
    // https://doc.rust-lang.org/std/process/struct.Child.html#method.wait
    // The stdin handle to the child process, if any, will be closed before waiting.
    // This helps avoid deadlock: it ensures that the child does not block waiting
    // for input from the parent, while the parent waits for the child to exit.
    unsafe { RECEIVER.as_mut().unwrap().wait().unwrap() };
    // TODO, revert to the default handler https://github.com/iximeow/rust/blob/28eeea630faf1e7514da96c5eedd67e330fe8571/src/libstd/sys/unix/stack_overflow.rs#L105
    std::process::abort();
}

//TODO, get other signals than segv?
fn register_crash_handler() -> anyhow::Result<()> {
    let sig_action = SigAction::new(
        SigHandler::Handler(handle_sigsegv),
        SaFlags::SA_NODEFER | SaFlags::SA_ONSTACK,
        signal::SigSet::empty(),
    );
    unsafe {
        set_alt_stack(0)?;
        // TODO, check if there was a previous handler?
        // TODO, store it in global variable, then restore it when our handler finishes?
        signal::sigaction(signal::SIGSEGV, &sig_action)?;
    }
    Ok(())
}

unsafe fn get_stack() -> anyhow::Result<libc::stack_t> {
    Ok(libc::stack_t {
        ss_sp: get_stackp()?,
        ss_flags: 0,
        ss_size: SIGSTKSZ,
    })
}

/// Allocates a signal altstack, and puts a guard page at the end.
/// Inspired by https://github.com/rust-lang/rust/pull/69969/files
unsafe fn get_stackp() -> anyhow::Result<*mut libc::c_void> {
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
    Ok(stackp.add(page_size))
}

// https://github.com/iximeow/rust/blob/28eeea630faf1e7514da96c5eedd67e330fe8571/src/libstd/sys/unix/stack_overflow.rs#L140
// https://github.com/rust-lang/rust/pull/69969/files
unsafe fn set_alt_stack(_size: usize) -> anyhow::Result<()> {
    let stack = get_stack()?;
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
