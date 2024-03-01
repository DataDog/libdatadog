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
use std::fs::File;
use std::io::Write;
use std::process::{Command, Stdio};
use std::ptr;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicBool, AtomicPtr};

#[derive(Debug)]
struct OldHandlers {
    sigbus: SigAction,
    sigsegv: SigAction,
}

// These represent data used by the crashtracker.
// Using mutexes inside a signal handler is not allowed, so use `AtomicPtr`
// instead to get atomicity.
// These should always be either: null_mut, or `Box::into_raw()`
// This means that we can always clean up the memory inside one of these using
// `Box::from_raw` to recreate the box, then dropping it.
static ALTSTACK_INIT: AtomicBool = AtomicBool::new(false);
static OLD_HANDLERS: AtomicPtr<OldHandlers> = AtomicPtr::new(ptr::null_mut());
static RECEIVER: AtomicPtr<std::process::Child> = AtomicPtr::new(ptr::null_mut());
static METADATA: AtomicPtr<(CrashtrackerMetadata, String)> = AtomicPtr::new(ptr::null_mut());
static CONFIG: AtomicPtr<(CrashtrackerConfiguration, String)> = AtomicPtr::new(ptr::null_mut());

fn make_receiver(config: CrashtrackerConfiguration) -> anyhow::Result<std::process::Child> {
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

    Ok(receiver)
}

/// Updates the crashtracker metadata for this process
/// Metadata is stored in a global variable and sent to the crashtracking
/// receiver when a crash occurs.
///
/// PRECONDITIONS:
///     None
/// SAFETY:
///     Crash-tracking functions are not guaranteed to be reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function uses a swap on an atomic pointer.
pub fn update_metadata(metadata: CrashtrackerMetadata) -> anyhow::Result<()> {
    let metadata_string = serde_json::to_string(&metadata)?;
    let box_ptr = Box::into_raw(Box::new((metadata, metadata_string)));
    let old = METADATA.swap(box_ptr, SeqCst);
    if !old.is_null() {
        // Safety: This can only come from a box above.
        unsafe {
            std::mem::drop(Box::from_raw(old));
        }
    }
    Ok(())
}

/// Updates the crashtracker config for this process
/// Config is stored in a global variable and sent to the crashtracking
/// receiver when a crash occurs.
///
/// PRECONDITIONS:
///     None
/// SAFETY:
///     Crash-tracking functions are not guaranteed to be reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function uses a swap on an atomic pointer.
pub fn update_config(config: CrashtrackerConfiguration) -> anyhow::Result<()> {
    let config_string = serde_json::to_string(&config)?;
    let box_ptr = Box::into_raw(Box::new((config, config_string)));
    let old = CONFIG.swap(box_ptr, SeqCst);
    if !old.is_null() {
        // Safety: This can only come from a box above.
        unsafe {
            std::mem::drop(Box::from_raw(old));
        }
    }
    Ok(())
}

/// Ensures there is a receiver running.
/// PRECONDITIONS:
///     None
/// SAFETY:
///     Crash-tracking functions are not guaranteed to be reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function uses a compare_and_exchange on an atomic pointer.
///     If two simultaneous calls to this function occur, the first will win,
///     and the second will cleanup the redundant receiver.
pub fn ensure_receiver(config: CrashtrackerConfiguration) -> anyhow::Result<()> {
    if !RECEIVER.load(SeqCst).is_null() {
        // Receiver already running
        return Ok(());
    }

    let new_receiver = Box::into_raw(Box::new(make_receiver(config)?));
    let res = RECEIVER.compare_exchange(ptr::null_mut(), new_receiver, SeqCst, SeqCst);
    if res.is_err() {
        // Race condition: Someone else setup the receiver between check and now.
        // Cleanup after ourselves
        // Safety: we just took it from a box above, and own the only ref since
        // the compare_exchange failed.
        let mut new_receiver = unsafe { Box::from_raw(new_receiver) };
        new_receiver.kill()?;
        new_receiver.wait()?;
    }

    Ok(())
}

/// Each fork needs its own receiver.  This function should run in the child
/// after a fork to spawn a new receiver for the child.
/// PRECONDITIONS:
///     None
/// SAFETY:
///     Crash-tracking functions are not guaranteed to be reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function uses a compare_and_exchange on an atomic pointer.
///     If two simultaneous calls to this function occur, the first will win,
///     and the second will cleanup the redundant receiver.
pub fn update_receiver_after_fork(config: CrashtrackerConfiguration) -> anyhow::Result<()> {
    let new_receiver = Box::into_raw(Box::new(make_receiver(config)?));
    let old_receiver: *mut std::process::Child = RECEIVER.swap(new_receiver, SeqCst);
    anyhow::ensure!(
        !old_receiver.is_null(),
        "Error updating crash handler receiver: receiver did not already exist"
    );
    // Safety: This was only ever created out of Box::into_raw
    let mut old_receiver = unsafe { Box::from_raw(old_receiver) };
    // Close the stdin handle so we don't have two open copies
    // TODO: dropping the old receiver at the end of this function might do this automatically?
    drop(old_receiver.stdin.take());
    drop(old_receiver.stdout.take());
    drop(old_receiver.stderr.take());
    // Leave the old one running, since its being used by another fork

    Ok(())
}

/// Shuts down a receiver,
/// PRECONDITIONS:
///     The signal handlers should be restored before removing the receiver.
/// SAFETY:
///     Crash-tracking functions are not guaranteed to be reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function uses a compare_and_exchange on an atomic pointer.
///     If two simultaneous calls to this function occur, the first will win,
///     and the second will cleanup the redundant receiver.
pub fn shutdown_receiver() -> anyhow::Result<()> {
    anyhow::ensure!(
        OLD_HANDLERS.load(SeqCst).is_null(),
        "Crashtracker signal handlers should removed before shutting down the receiver"
    );
    let old_receiver = RECEIVER.swap(ptr::null_mut(), SeqCst);
    if !old_receiver.is_null() {
        // Safety: This only comes from a `Box::into_raw`, and was checked for
        // null above
        let mut old_receiver = unsafe { Box::from_raw(old_receiver) };
        old_receiver.kill()?;
        old_receiver.wait()?;
    }
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

fn emit_config(w: &mut impl Write, config_str: &str) -> anyhow::Result<()> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_CONFIG}")?;
    writeln!(w, "{}", config_str)?;
    writeln!(w, "{DD_CRASHTRACK_END_CONFIG}")?;
    Ok(())
}

fn emit_metadata(w: &mut impl Write, metadata_str: &str) -> anyhow::Result<()> {
    writeln!(w, "{DD_CRASHTRACK_BEGIN_METADATA}")?;
    writeln!(w, "{}", metadata_str)?;
    writeln!(w, "{DD_CRASHTRACK_END_METADATA}")?;
    Ok(())
}

fn emit_siginfo(w: &mut impl Write, signum: i32) -> anyhow::Result<()> {
    let signame = if signum == libc::SIGSEGV {
        "SIGSEGV"
    } else if signum == libc::SIGBUS {
        "SIGBUS"
    } else {
        "UNKNOWN"
    };

    writeln!(w, "{DD_CRASHTRACK_BEGIN_SIGINFO}")?;
    writeln!(w, "{{\"signum\": {signum}, \"signame\": \"{signame}\"}}")?;
    writeln!(w, "{DD_CRASHTRACK_END_SIGINFO}")?;
    Ok(())
}

fn handle_posix_signal_impl(signum: i32) -> anyhow::Result<()> {
    // Leak receiver, config, and metadata to avoid calling 'drop' during a crash
    let receiver = RECEIVER.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(!receiver.is_null(), "No crashtracking receiver");
    let receiver = unsafe { receiver.as_mut().context("No crashtracking receiver")? };

    let config = CONFIG.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(!config.is_null(), "No crashtracking config");
    let (config, config_str) = unsafe { config.as_ref().context("No crashtracking receiver")? };

    let metadata_ptr = METADATA.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(!metadata_ptr.is_null(), "No crashtracking metadata");
    let (_metadata, metadata_string) = unsafe { metadata_ptr.as_ref().context("metadata ptr")? };

    let pipe = receiver
        .stdin
        .as_mut()
        .context("Crashtracker: Can't get pipe")?;

    emit_metadata(pipe, metadata_string)?;
    emit_config(pipe, config_str)?;
    emit_siginfo(pipe, signum)?;
    pipe.flush()?;
    emit_counters(pipe)?;
    pipe.flush()?;

    #[cfg(target_os = "linux")]
    emit_proc_self_maps(pipe)?;

    // Getting a backtrace on rust is not guaranteed to be signal safe
    // https://github.com/rust-lang/backtrace-rs/issues/414
    // let current_backtrace = backtrace::Backtrace::new();
    // In fact, if we look into the code here, we see mallocs.
    // https://doc.rust-lang.org/src/std/backtrace.rs.html#332
    // Do this last, so even if it crashes, we still get the other info.
    if config.collect_stacktrace {
        unsafe {
            emit_backtrace_by_frames(
                pipe,
                config.resolve_frames == CrashtrackerResolveFrames::ExperimentalInProcess,
            )?
        };
    }
    writeln!(pipe, "{DD_CRASHTRACK_DONE}")?;

    pipe.flush()?;
    // https://doc.rust-lang.org/std/process/struct.Child.html#method.wait
    // The stdin handle to the child process, if any, will be closed before waiting.
    // This helps avoid deadlock: it ensures that the child does not block waiting
    // for input from the parent, while the parent waits for the child to exit.
    // TODO, use a polling mechanism that could recover from a crashing child
    receiver.wait()?;
    // Calling "free" in a signal handler is dangerous, so we just leak the
    // objects we took (receiver, metadata, config, etc)
    Ok(())
}

/// Registers UNIX signal handlers to detect program crashes.
/// This function can be called multiple times and will be idempotent: it will
/// only create and set the handlers once.
/// However, note the restriction below:
/// PRECONDITIONS:
///     The signal handlers should be restored before removing the receiver.
/// SAFETY:
///     Crash-tracking functions are not guaranteed to be reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function uses a compare_and_exchange on an atomic pointer.
///     However, setting the crash handler itself is not an atomic operation
///     and hence it is possible that a concurrent operation could see partial
///     execution of this function.
///     If a crash occurs during execution of this function, it is possible that
///     the crash handler will have been registered, but the old signal handler
///     will not yet be stored.  This would lead to unexpected behaviour for the
///     user.  This should only matter if something crashes concurrently with
///     this function executing.
pub fn register_crash_handlers(create_alt_stack: bool) -> anyhow::Result<()> {
    if !OLD_HANDLERS.load(SeqCst).is_null() {
        return Ok(());
    }

    unsafe {
        if create_alt_stack {
            set_alt_stack()?;
        }
        let sigbus = register_signal_handler(signal::SIGBUS)?;
        let sigsegv = register_signal_handler(signal::SIGSEGV)?;
        let boxed_ptr = Box::into_raw(Box::new(OldHandlers { sigbus, sigsegv }));

        let res = OLD_HANDLERS.compare_exchange(ptr::null_mut(), boxed_ptr, SeqCst, SeqCst);
        anyhow::ensure!(
            res.is_ok(),
            "TOCTTOU error in crashtracker::register_crash_handlers"
        );
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

pub fn restore_old_handlers(inside_signal_handler: bool) -> anyhow::Result<()> {
    let prev = OLD_HANDLERS.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(!prev.is_null(), "No crashtracking previous signal handlers");
    // Safety: The only nonnull pointer stored here comes from Box::into_raw()
    let prev = unsafe { Box::from_raw(prev) };
    // Safety: The value restored here was returned from a previous sigaction call
    unsafe { signal::sigaction(signal::SIGBUS, &prev.sigbus)? };
    unsafe { signal::sigaction(signal::SIGSEGV, &prev.sigsegv)? };
    // We want to avoid freeing memory inside the handler, so just leak it
    // This is fine since we're crashing anyway at this point
    if inside_signal_handler {
        Box::leak(prev);
    }
    Ok(())
}

/// Allocates a signal altstack, and puts a guard page at the end.
/// Inspired by https://github.com/rust-lang/rust/pull/69969/files
unsafe fn set_alt_stack() -> anyhow::Result<()> {
    if ALTSTACK_INIT.load(SeqCst) {
        return Ok(());
    }

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
    ALTSTACK_INIT.store(true, SeqCst);
    Ok(())
}
