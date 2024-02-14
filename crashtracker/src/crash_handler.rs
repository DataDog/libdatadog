// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

#![cfg(unix)]

use super::api::{CrashtrackerConfiguration, CrashtrackerMetadata, CrashtrackerResolveFrames};
use super::collectors::emit_backtrace_by_frames;
#[cfg(target_os = "linux")]
use super::collectors::emit_proc_self_maps;
use super::constants::*;
use super::counters::emit_counters;
use anyhow::Context;
use libc::{
    mmap, sigaltstack, MAP_ANON, MAP_FAILED, MAP_PRIVATE, PROT_NONE, PROT_READ, PROT_WRITE,
    SIGSTKSZ,
};
use nix::sys::signal;
use nix::sys::signal::{SaFlags, SigAction, SigHandler};
use std::fs::{File, Metadata};
use std::io::Write;
use std::process::{Command, Stdio};
use std::ptr;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering::SeqCst;

#[derive(Debug)]
struct OldHandlers {
    sigbus: SigAction,
    sigsegv: SigAction,
}

static OLD_HANDLERS: AtomicPtr<OldHandlers> = AtomicPtr::new(ptr::null_mut());
static RECEIVER: AtomicPtr<std::process::Child> = AtomicPtr::new(ptr::null_mut());
static METADATA: AtomicPtr<(CrashtrackerMetadata, String)> = AtomicPtr::new(ptr::null_mut());
static CONFIG: AtomicPtr<(CrashtrackerConfiguration, String)> = AtomicPtr::new(ptr::null_mut());

static mut RESOLVE_FRAMES: bool = false;
static mut METADATA_STRING: Option<String> = None;

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

    update_metadata(metadata)?;
    Ok(receiver)
}

/// Updates the crashtracker metadata for this process
/// Metadata is stored in a global variable and sent to the crashtracking
/// receiver when a crash occurs.
///
/// PRECONDITIONS:
///     None
/// SAFETY:
///     Crash-tracking functions are not reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function is not atomic. A crash during its execution may lead to
///     unexpected crash-handling behaviour.
pub fn update_metadata(metadata: &CrashtrackerMetadata) -> anyhow::Result<()> {
    let metadata_string = serde_json::to_string(&metadata)?;
    unsafe { METADATA_STRING = Some(metadata_string) };
    Ok(())
}

pub fn setup_receiver(
    config: &CrashtrackerConfiguration,
    metadata: &CrashtrackerMetadata,
) -> anyhow::Result<()> {
    let new_receiver = Box::into_raw(Box::new(make_receiver(config, metadata)?));
    let old_receiver = RECEIVER.swap(new_receiver, SeqCst);
    anyhow::ensure!(
        old_receiver.is_null(),
        "Error registering crash handler receiver: receiver already existed"
    );
    Ok(())
}

pub fn replace_receiver(
    config: &CrashtrackerConfiguration,
    metadata: &CrashtrackerMetadata,
) -> anyhow::Result<()> {
    let new_receiver = Box::into_raw(Box::new(make_receiver(config, metadata)?));
    let old_receiver: *mut std::process::Child = RECEIVER.swap(new_receiver, SeqCst);
    anyhow::ensure!(
        !old_receiver.is_null(),
        "Error updating crash handler receiver: receiver did not already exist"
    );
    // Safety: This was only ever created out of Box::into_raw
    let mut old_receiver: Box<std::process::Child> = unsafe { Box::from_raw(old_receiver) };
    // Close the stdin handle so we don't have two open copies
    // TODO: dropping the old receiver at the end of this function might do this automatically?
    drop(old_receiver.stdin.take());
    drop(old_receiver.stdout.take());
    drop(old_receiver.stderr.take());
    // Leave the old one running, since its being used by another fork

    Ok(())
}

pub fn shutdown_receiver() -> anyhow::Result<()> {
    let old_receiver = RECEIVER.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(
        !old_receiver.is_null(),
        "Error shutting down crash handler receiver: receiver did not already exist"
    );
    let mut old_receiver = unsafe { Box::from_raw(old_receiver) };

    old_receiver.kill()?;
    old_receiver.wait()?;
    Ok(())
}

extern "C" fn handle_posix_signal(signum: i32) {
    // Safety: We've already crashed, this is a best effort to chain to the old
    // behaviour.  Do this first to prevent recursive activation if this handler
    // itself crashes (e.g. while calculating stacktrace)
    let _ = restore_old_handlers(true);
    let _ = handle_posix_signal_impl(signum);

    // return to old handler (chain).  See comments on `restore_old_handler`.
}

fn emit_metadata(w: &mut impl Write) -> anyhow::Result<()> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_METADATA}")?;

    let metadata = unsafe { &METADATA_STRING.as_ref().context("Expected metadata")? };
    writeln!(w, "{}", metadata)?;

    writeln!(w, "{DD_CRASHTRACK_END_METADATA}")?;

    Ok(())
}

fn handle_posix_signal_impl(signum: i32) -> anyhow::Result<()> {
    let receiver = RECEIVER.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(!receiver.is_null(), "No crashtracking receiver");
    let receiver = unsafe { receiver.as_mut().context("")? };

    let signame = if signum == libc::SIGSEGV {
        "SIGSEGV"
    } else if signum == libc::SIGBUS {
        "SIGBUS"
    } else {
        "UNKNOWN"
    };

    let pipe = receiver.stdin.as_mut().unwrap();

    emit_metadata(pipe)?;

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
    writeln!(pipe, "{DD_CRASHTRACK_DONE}")?;

    pipe.flush()?;
    // https://doc.rust-lang.org/std/process/struct.Child.html#method.wait
    // The stdin handle to the child process, if any, will be closed before waiting.
    // This helps avoid deadlock: it ensures that the child does not block waiting
    // for input from the parent, while the parent waits for the child to exit.
    // TODO, use a polling mechanism that could recover from a crashing child
    receiver.wait()?;
    // Calling "free" in a signal handler is dangerous, so don't do that.
    Ok(())
}

// TODO, there is a small race condition here, but we can keep it small
pub fn register_crash_handlers(config: &CrashtrackerConfiguration) -> anyhow::Result<()> {
    anyhow::ensure!(OLD_HANDLERS.load(SeqCst).is_null());
    unsafe {
        RESOLVE_FRAMES = config.resolve_frames == CrashtrackerResolveFrames::ExperimentalInProcess;

        if config.create_alt_stack {
            set_alt_stack()?;
        }
        let sigbus = register_signal_handler(signal::SIGBUS)?;
        let sigsegv = register_signal_handler(signal::SIGSEGV)?;
        let boxed_ptr = Box::into_raw(Box::new(OldHandlers { sigbus, sigsegv }));

        let res = OLD_HANDLERS.compare_exchange(ptr::null_mut(), boxed_ptr, SeqCst, SeqCst);
        anyhow::ensure!(res.is_ok());
    }
    Ok(())
}

unsafe fn register_signal_handler(signal_type: signal::Signal) -> anyhow::Result<SigAction> {
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
    Ok(old_handler)
}

pub fn restore_old_handlers(leak: bool) -> anyhow::Result<()> {
    let prev = OLD_HANDLERS.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(!prev.is_null());
    // Safety: The only nonnull pointer stored here comes from Box::into_raw()
    let prev = unsafe { Box::from_raw(prev) };
    // Safety: The value restored here was returned from a previous sigaction call
    unsafe { signal::sigaction(signal::SIGBUS, &prev.sigbus)? };
    unsafe { signal::sigaction(signal::SIGSEGV, &prev.sigsegv)? };
    // We want to avoid freeing memory inside the handler, so just leak it 
    // This is fine since we're crashing anyway at this point
    if leak {
        Box::leak(prev);
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
