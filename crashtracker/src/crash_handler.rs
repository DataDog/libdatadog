// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

#![cfg(unix)]

use std::io::Write;
use std::ptr;

#[cfg(target_os = "linux")]
use super::collectors::emit_proc_self_maps;

use super::api::{CrashtrackerConfiguration, CrashtrackerMetadata, CrashtrackerResolveFrames};
use super::collectors::emit_backtrace_by_frames;
use super::constants::*;
use super::counters::emit_counters;
use anyhow::Context;
use libc::{
    mmap, sigaltstack, MAP_ANON, MAP_FAILED, MAP_PRIVATE, PROT_NONE, PROT_READ, PROT_WRITE,
    SIGSTKSZ,
};
use nix::sys::signal;
use nix::sys::signal::{SaFlags, SigAction, SigHandler};
use std::fs::File;
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
static mut OLD_SIGBUS_HANDLER: GlobalVarState<SigAction> = GlobalVarState::Unassigned;
static mut OLD_SIGSEGV_HANDLER: GlobalVarState<SigAction> = GlobalVarState::Unassigned;
static mut RESOLVE_FRAMES: bool = false;

fn make_receiver(
    config: &CrashtrackerConfiguration,
    metadata: &CrashtrackerMetadata,
) -> anyhow::Result<std::process::Child> {
    // TODO: currently create the file in write mode.  Would append make more sense?
    let stderr = if let Some(filename) = &config.stderr_filename {
        File::create(filename)?.into()
    } else {
        Stdio::null()
    };

    let stdout = if let Some(filename) = &config.stdout_filename {
        File::create(filename)?.into()
    } else {
        Stdio::null()
    };

    let receiver = Command::new(&config.path_to_receiver_binary)
        .arg("receiver")
        .stdin(Stdio::piped())
        .stderr(stderr)
        .stdout(stdout)
        .spawn()
        .context(format!(
            "Unable to start process: {}",
            &config.path_to_receiver_binary
        ))?;

    // Write the args into the receiver.
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

pub fn setup_receiver(
    config: &CrashtrackerConfiguration,
    metadata: &CrashtrackerMetadata,
) -> anyhow::Result<()> {
    let new_receiver = make_receiver(config, metadata)?;
    let old_receiver =
        unsafe { std::mem::replace(&mut RECEIVER, GlobalVarState::Some(new_receiver)) };
    anyhow::ensure!(
        matches!(old_receiver, GlobalVarState::Unassigned),
        "Error registering crash handler receiver: receiver already existed {old_receiver:?}"
    );

    Ok(())
}

pub fn replace_receiver(
    config: &CrashtrackerConfiguration,
    metadata: &CrashtrackerMetadata,
) -> anyhow::Result<()> {
    let new_receiver = make_receiver(config, metadata)?;
    let old_receiver =
        unsafe { std::mem::replace(&mut RECEIVER, GlobalVarState::Some(new_receiver)) };
    if let GlobalVarState::Some(mut old_receiver) = old_receiver {
        // Close the stdin handle so we don't have two open copies
        // TODO: dropping the old receiver at the end of this function might do this automatically?
        drop(old_receiver.stdin.take());
        drop(old_receiver.stdout.take());
        drop(old_receiver.stderr.take());
        // Leave the old one running, since its being used by another fork
    } else {
        anyhow::bail!(
            "Error updating crash handler receiver: receiver did not already exist {old_receiver:?}"
        );
    }

    Ok(())
}

pub fn shutdown_receiver() -> anyhow::Result<()> {
    let old_receiver = unsafe { std::mem::replace(&mut RECEIVER, GlobalVarState::Taken) };
    if let GlobalVarState::Some(mut old_receiver) = old_receiver {
        old_receiver.kill()?;
        old_receiver.wait()?;
    } else {
        anyhow::bail!(
            "Error shutting down crash handler receiver: receiver did not already exist {old_receiver:?}"
        );
    }
    Ok(())
}

extern "C" fn handle_posix_signal(signum: i32) {
    // Safety: We've already crashed, this is a best effort to chain to the old
    // behaviour.  Do this first to prevent recursive activation if this handler
    // itself crashes (e.g. while calculating stacktrace)
    let _ = restore_old_handlers();
    let _ = handle_posix_signal_impl(signum);

    // return to old handler (chain).  See comments on `restore_old_handler`.
}

fn handle_posix_signal_impl(signum: i32) -> anyhow::Result<()> {
    let mut receiver = match std::mem::replace(unsafe { &mut RECEIVER }, GlobalVarState::Taken) {
        GlobalVarState::Some(r) => r,
        GlobalVarState::Unassigned => anyhow::bail!("Cannot acquire receiver: Unassigned"),
        GlobalVarState::Taken => anyhow::bail!("Cannot acquire receiver: Taken"),
    };

    let signame = if signum == libc::SIGSEGV {
        "SIGSEGV"
    } else if signum == libc::SIGBUS {
        "SIGBUS"
    } else {
        "UNKNOWN"
    };

    let pipe = receiver.stdin.as_mut().unwrap();
    writeln!(pipe, "{DD_CRASHTRACK_BEGIN_SIGINFO}")?;
    writeln!(pipe, "{{\"signum\": {signum}, \"signame\": \"{signame}\"}}")?;
    writeln!(pipe, "{DD_CRASHTRACK_END_SIGINFO}")?;

    emit_counters(pipe)?;

    #[cfg(target_os = "linux")]
    emit_proc_self_maps(pipe)?;

    // Getting a backtrace on rust is not guaranteed to be signal safe
    // https://github.com/rust-lang/backtrace-rs/issues/414
    // let current_backtrace = backtrace::Backtrace::new();
    // In fact, if we look into the code here, we see mallocs.
    // https://doc.rust-lang.org/src/std/backtrace.rs.html#332
    // Do this last, so even if it crashes, we still get the other info.
    unsafe { emit_backtrace_by_frames(pipe, RESOLVE_FRAMES)? };

    pipe.flush()?;
    // https://doc.rust-lang.org/std/process/struct.Child.html#method.wait
    // The stdin handle to the child process, if any, will be closed before waiting.
    // This helps avoid deadlock: it ensures that the child does not block waiting
    // for input from the parent, while the parent waits for the child to exit.
    //TODO, use a polling mechanism that could recover from a crashing child
    receiver.wait()?;
    Ok(())
}

pub fn register_crash_handlers(config: &CrashtrackerConfiguration) -> anyhow::Result<()> {
    unsafe {
        RESOLVE_FRAMES = config.resolve_frames == CrashtrackerResolveFrames::ExperimentalInProcess;

        if config.create_alt_stack {
            set_alt_stack()?;
        }
        register_signal_handler(signal::SIGBUS)?;
        register_signal_handler(signal::SIGSEGV)?;
    }
    Ok(())
}

unsafe fn get_slot(
    signal_type: signal::Signal,
) -> anyhow::Result<&'static mut GlobalVarState<SigAction>> {
    let slot = match signal_type {
        signal::SIGBUS => unsafe { &mut OLD_SIGBUS_HANDLER },
        signal::SIGSEGV => unsafe { &mut OLD_SIGSEGV_HANDLER },
        _ => anyhow::bail!("unexpected signal {signal_type}"),
    };
    Ok(slot)
}

unsafe fn register_signal_handler(signal_type: signal::Signal) -> anyhow::Result<()> {
    let slot = get_slot(signal_type)?;

    // https://www.gnu.org/software/libc/manual/html_node/Flags-for-Sigaction.html
    // ===============
    // If this flag is set for a particular signal number, the system uses the
    // signal stack when delivering that kind of signal.
    // See Using a Separate Signal Stack.
    // If a signal with this flag arrives and you have not set a signal stack,
    // the normal user stack is used instead, as if the flag had not been set.
    // ===============
    // This implies that it is always safe to set SA_ONSTACK.
    let sig_action = SigAction::new(
        //SigHandler::SigAction(_handle_sigsegv_info),
        SigHandler::Handler(handle_posix_signal),
        SaFlags::SA_NODEFER | SaFlags::SA_ONSTACK,
        signal::SigSet::empty(),
    );

    let old_handler = signal::sigaction(signal_type, &sig_action)?;
    let should_be_empty = std::mem::replace(slot, GlobalVarState::Some(old_handler));
    anyhow::ensure!(
        matches!(should_be_empty, GlobalVarState::Unassigned),
        "Error registering crash handler: old_handler already existed {should_be_empty:?}"
    );

    Ok(())
}

unsafe fn replace_signal_handler(signal_type: signal::Signal) -> anyhow::Result<()> {
    let slot = get_slot(signal_type)?;

    match std::mem::replace(slot, GlobalVarState::Taken) {
        GlobalVarState::Some(old_handler) => unsafe {
            signal::sigaction(signal_type, &old_handler)?
        },
        x => anyhow::bail!("Cannot restore signal handler for {signal_type}: {x:?}"),
    };
    Ok(())
}

pub fn restore_old_handlers() -> anyhow::Result<()> {
    // Restore the old handler, so that the current handler can return to it.
    // Although this is technically UB, this is what Rust does in the same case.
    // https://github.com/rust-lang/rust/blob/master/library/std/src/sys/unix/stack_overflow.rs#L75
    unsafe {
        replace_signal_handler(signal::SIGSEGV)?;
        replace_signal_handler(signal::SIGBUS)?;
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
