// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use std::io::Write;
use std::ptr;

use super::api::{Configuration, Metadata};
use super::collectors::{emit_backtrace_by_frames, emit_proc_self_maps};
use super::constants::*;
use super::counters::emit_counters;
use libc::{
    mmap, sigaltstack, MAP_ANON, MAP_FAILED, MAP_PRIVATE, PROT_NONE, PROT_READ, PROT_WRITE,
    SIGSTKSZ,
};
use nix::sys::signal;
use nix::sys::signal::{SaFlags, SigAction, SigHandler};
use std::process::{Command, Stdio};

#[derive(Debug)]
enum GlobalVarState<T>
where
    T: std::fmt::Debug,
{
    Unassigned,
    Some(T),
    Taken,
}

static mut RECEIVER: GlobalVarState<std::process::Child> = GlobalVarState::Unassigned;
static mut OLD_HANDLER: GlobalVarState<SigAction> = GlobalVarState::Unassigned;

fn make_reciever(
    config: &Configuration,
    metadata: &Metadata,
) -> anyhow::Result<std::process::Child> {
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
    Ok(receiver)
}

pub fn setup_receiver(config: &Configuration, metadata: &Metadata) -> anyhow::Result<()> {
    let new_reciever = make_reciever(config, metadata)?;
    let old_reciever =
        unsafe { std::mem::replace(&mut RECEIVER, GlobalVarState::Some(new_reciever)) };
    anyhow::ensure!(
        matches!(old_reciever, GlobalVarState::Unassigned),
        "Error registering crash handler reciever: reciever already existed {old_reciever:?}"
    );

    Ok(())
}

pub fn replace_receiver(config: &Configuration, metadata: &Metadata) -> anyhow::Result<()> {
    let new_reciever = make_reciever(config, metadata)?;
    let old_reciever =
        unsafe { std::mem::replace(&mut RECEIVER, GlobalVarState::Some(new_reciever)) };
    if let GlobalVarState::Some(mut old_reciever) = old_reciever {
        // Close the stdin handle so we don't have two open copies
        // TODO: dropping the old reciever at the end of this function might do this automatically?
        drop(old_reciever.stdin.take());
        drop(old_reciever.stdout.take());
        drop(old_reciever.stderr.take());
        // Leave the old one running, since its being used by another fork
    } else {
        anyhow::bail!(
            "Error updating crash handler reciever: reciever did not already exist {old_reciever:?}"
        );
    }

    Ok(())
}

pub fn shutdown_receiver() -> anyhow::Result<()> {
    let old_reciever = unsafe { std::mem::replace(&mut RECEIVER, GlobalVarState::Taken) };
    if let GlobalVarState::Some(mut old_reciever) = old_reciever {
        old_reciever.kill()?;
        old_reciever.wait()?;
    } else {
        anyhow::bail!(
            "Error shutting down crash handler reciever: reciever did not already exist {old_reciever:?}"
        );
    }
    Ok(())
}

extern "C" fn handle_sigsegv(signum: i32) {
    // Safety: We've already crashed, this is a best effort to chain to the old
    // behaviour.  Do this first to prevent recursive activation if this handler
    // itself crases (e.g. while calculating stacktrace)
    let _ = restore_old_handler();
    let _ = handle_sigsegv_impl(signum);

    // return to old handler (chain).  See comments on `restore_old_handler`.
}

fn handle_sigsegv_impl(signum: i32) -> anyhow::Result<()> {
    let mut receiver = match std::mem::replace(unsafe { &mut RECEIVER }, GlobalVarState::Taken) {
        GlobalVarState::Some(r) => r,
        GlobalVarState::Unassigned => anyhow::bail!("Cannot find receiver: Unassigned"),
        GlobalVarState::Taken => anyhow::bail!("Cannot receiver: Taken"),
    };

    let pipe = receiver.stdin.as_mut().unwrap();
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
    //TODO, use a polling mechanism that could recover from a crashing child
    receiver.wait()?;
    Ok(())
}

//TODO, get other signals than segv?
pub fn register_crash_handler() -> anyhow::Result<()> {
    let sig_action = SigAction::new(
        //SigHandler::SigAction(_handle_sigsegv_info),
        SigHandler::Handler(handle_sigsegv),
        SaFlags::SA_NODEFER | SaFlags::SA_ONSTACK,
        signal::SigSet::empty(),
    );
    unsafe {
        set_alt_stack()?;
        let old = signal::sigaction(signal::SIGSEGV, &sig_action)?;
        let prev_old_handler = std::mem::replace(&mut OLD_HANDLER, GlobalVarState::Some(old));
        anyhow::ensure!(
            matches!(prev_old_handler, GlobalVarState::Unassigned),
            "Error registering crash handler: old_handler already existed {prev_old_handler:?}"
        );
    }
    Ok(())
}

pub fn restore_old_handler() -> anyhow::Result<()> {
    // Restore the old handler, so that the current handler can return to it.
    // Although this is technically UB, this is what Rust does in the same case.
    // https://github.com/rust-lang/rust/blob/master/library/std/src/sys/unix/stack_overflow.rs#L75
    match std::mem::replace(unsafe { &mut OLD_HANDLER }, GlobalVarState::Taken) {
        GlobalVarState::Some(old_handler) => {
            unsafe { signal::sigaction(signal::SIGSEGV, &old_handler) }?
        }
        x => anyhow::bail!("Cannot restore signal handler: {x:?}"),
    };
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
