// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

use super::emitters::emit_crashreport;
use crate::crash_info::CrashtrackerMetadata;
use crate::shared::configuration::{CrashtrackerConfiguration, CrashtrackerReceiverConfig};
use anyhow::Context;
use libc::{
    c_void, mmap, sigaltstack, siginfo_t, MAP_ANON, MAP_FAILED, MAP_PRIVATE, PROT_NONE, PROT_READ,
    PROT_WRITE, SIGSTKSZ,
};
use nix::sys::signal;
use nix::sys::signal::{SaFlags, SigAction, SigHandler};
use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::ptr;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64};

#[derive(Debug)]
struct OldHandlers {
    pub sigbus: SigAction,
    pub sigsegv: SigAction,
}

enum ReceiverType {
    ForkedProcess(std::process::Child),
    UnixSocket(String),
}

// These represent data used by the crashtracker.
// Using mutexes inside a signal handler is not allowed, so use `AtomicPtr`
// instead to get atomicity.
// These should always be either: null_mut, or `Box::into_raw()`
// This means that we can always clean up the memory inside one of these using
// `Box::from_raw` to recreate the box, then dropping it.
static ALTSTACK_INIT: AtomicBool = AtomicBool::new(false);
static OLD_HANDLERS: AtomicPtr<OldHandlers> = AtomicPtr::new(ptr::null_mut());
static RECEIVER: AtomicPtr<ReceiverType> = AtomicPtr::new(ptr::null_mut());
static METADATA: AtomicPtr<(CrashtrackerMetadata, String)> = AtomicPtr::new(ptr::null_mut());
static CONFIG: AtomicPtr<(CrashtrackerConfiguration, String)> = AtomicPtr::new(ptr::null_mut());

fn make_receiver(config: &CrashtrackerReceiverConfig) -> anyhow::Result<std::process::Child> {
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
        .args(&config.args)
        .envs(config.env.clone())
        .stdin(Stdio::piped())
        .stderr(stderr)
        .stdout(stdout)
        .spawn()
        .context(format!(
            "Unable to start process: {}",
            config.path_to_receiver_binary
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
pub fn ensure_receiver(config: &CrashtrackerReceiverConfig) -> anyhow::Result<()> {
    //TODO, this only really checks that we had something here, could be a unix socket.  Do we
    // care?
    if !RECEIVER.load(SeqCst).is_null() {
        // Receiver already running
        return Ok(());
    }

    let new_receiver = Box::into_raw(Box::new(ReceiverType::ForkedProcess(make_receiver(
        config,
    )?)));

    if RECEIVER
        .compare_exchange(ptr::null_mut(), new_receiver, SeqCst, SeqCst)
        .is_err()
    {
        // Safety: The receiver was created above from Box::into_raw, and this is the only reference
        // to it
        unsafe { cleanup_receiver(new_receiver)? };
    }

    Ok(())
}

pub fn socket_is_writable(_socket_path: &str) -> bool {
    // TODO, implement this
    true
}

/// Safety: Can only be called once, on a receiver type that came from Box::into_raw
unsafe fn cleanup_receiver(receiver: *mut ReceiverType) -> anyhow::Result<()> {
    if receiver.is_null() {
        return Ok(());
    }
    // Cleanup after ourselves (extracting back into a box ensures it will
    // be dropped when we return).
    // Safety: we just took it from a box above, and own the only ref since
    // the compare_exchange failed.
    let receiver = unsafe { Box::from_raw(receiver) };
    match *receiver {
        ReceiverType::ForkedProcess(mut child) => {
            child.kill()?;
            child.wait()?;
        }
        ReceiverType::UnixSocket(_) => (),
    };
    Ok(())
}

pub fn ensure_socket(socket_path: &str) -> anyhow::Result<()> {
    anyhow::ensure!(socket_is_writable(socket_path));
    let socket_path_ptr =
        Box::into_raw(Box::new(ReceiverType::UnixSocket(socket_path.to_string())));
    let old = RECEIVER.swap(socket_path_ptr, SeqCst);
    // Safety: the only thing that writes into the RECEIVER gets from a Box::into_raw, and puts
    // its only reference into it.
    unsafe { cleanup_receiver(old) }
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
pub fn update_receiver_after_fork(config: &CrashtrackerReceiverConfig) -> anyhow::Result<()> {
    let new_receiver = Box::into_raw(Box::new(ReceiverType::ForkedProcess(make_receiver(
        config,
    )?)));
    let old_receiver = RECEIVER.swap(new_receiver, SeqCst);
    anyhow::ensure!(
        !old_receiver.is_null(),
        "Error updating crash handler receiver: receiver did not already exist"
    );
    // Safety: This was only ever created out of Box::into_raw
    let old_receiver = unsafe { Box::from_raw(old_receiver) };
    match *old_receiver {
        ReceiverType::ForkedProcess(mut old_receiver) => {
            // Close the stdin handle so we don't have two open copies
            // TODO: dropping the old receiver at the end of this function might do this
            // automatically?
            drop(old_receiver.stdin.take());
            drop(old_receiver.stdout.take());
            drop(old_receiver.stderr.take());
            // Leave the old one running, since its being used by another fork
        }
        ReceiverType::UnixSocket(path) => {
            anyhow::bail!("tried to update crashtracker receiver process after fork, but the target was actually a unix socket: {path}")
        }
    }

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
    // Safety: This only comes from a `Box::into_raw`, and is the only example
    unsafe { cleanup_receiver(old_receiver) }
}

extern "C" fn handle_posix_sigaction(signum: i32, sig_info: *mut siginfo_t, ucontext: *mut c_void) {
    // Handle the signal.  Note this has a guard to ensure that we only generate
    // one crash report per process.
    let _ = handle_posix_signal_impl(signum);

    // Once we've handled the signal, chain to any previous handlers.
    // SAFETY: This was created by [register_crash_handlers].  There is a tiny
    // instant of time between when the handlers are registered, and the
    // `OLD_HANDLERS` are set.  This should be very short, but is hard to fully
    // eliminate given the existing POSIX APIs.
    let old_handlers = unsafe { &*OLD_HANDLERS.load(SeqCst) };
    let old_sigaction = if signum == libc::SIGSEGV {
        old_handlers.sigsegv
    } else if signum == libc::SIGBUS {
        old_handlers.sigbus
    } else {
        unreachable!("The only signals we're registered for are SEGV and BUS")
    };

    // How we chain depends on what kind of handler we're chaining to.
    // https://www.gnu.org/software/libc/manual/html_node/Signal-Handling.html
    // https://man7.org/linux/man-pages/man2/sigaction.2.html
    // Follow the approach here:
    // https://stackoverflow.com/questions/6015498/executing-default-signal-handler
    match old_sigaction.handler() {
        SigHandler::SigDfl => {
            // In the case of a default handler, we want to invoke it so that
            // the core-dump can be generated.  Restoring the handler then
            // re-raising the signal accomplishes that.
            let signal = if signum == libc::SIGSEGV {
                signal::SIGSEGV
            } else if signum == libc::SIGBUS {
                signal::SIGBUS
            } else {
                unreachable!("The only signals we're registered for are SEGV and BUS")
            };
            unsafe { signal::sigaction(signal, &old_sigaction) }
                .unwrap_or_else(|_| std::process::abort());
            // Signals are only delivered once.
            // In the case where we were invoked because of a crash, returning
            // is technically UB but in practice re-invokes the crashing instr
            // and re-raises the signal. In the case where we were invoked by
            // `raise(SIGSEGV)` we need to re-raise the signal, or the default
            // handler will never receive it.
            unsafe { libc::raise(signum) };
        }
        SigHandler::SigIgn => (), // Return and ignore the signal.
        SigHandler::Handler(f) => f(signum),
        SigHandler::SigAction(f) => f(signum, sig_info, ucontext),
    };
}

fn handle_posix_signal_impl(signum: i32) -> anyhow::Result<()> {
    static NUM_TIMES_CALLED: AtomicU64 = AtomicU64::new(0);
    if NUM_TIMES_CALLED.fetch_add(1, SeqCst) > 0 {
        // In the case where some lower-level signal handler recovered the error
        // we don't want to spam the system with calls.  Make this one shot.
        return Ok(());
    }

    // Leak receiver to avoid calling 'drop' during a crash
    let receiver = RECEIVER.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(!receiver.is_null(), "No crashtracking receiver");

    // Leak config, and metadata to avoid calling 'drop' during a crash
    let config = CONFIG.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(!config.is_null(), "No crashtracking config");
    let (config, config_str) = unsafe { config.as_ref().context("No crashtracking receiver")? };

    let metadata_ptr = METADATA.swap(ptr::null_mut(), SeqCst);
    anyhow::ensure!(!metadata_ptr.is_null(), "No crashtracking metadata");
    let (_metadata, metadata_string) = unsafe { metadata_ptr.as_ref().context("metadata ptr")? };

    match unsafe { receiver.as_mut().context("No crashtracking receiver")? } {
        ReceiverType::ForkedProcess(child) => {
            let pipe = child
                .stdin
                .as_mut()
                .context("Crashtracker: Can't get pipe")?;
            let res = emit_crashreport(pipe, config, config_str, metadata_string, signum);
            let _ = pipe.flush();
            if config.wait_for_receiver {
                // https://doc.rust-lang.org/std/process/struct.Child.html#method.wait
                // The stdin handle to the child process, if any, will be closed before waiting.
                // This helps avoid deadlock: it ensures that the child does not block waiting
                // for input from the parent, while the parent waits for the child to exit.
                // TODO, use a polling mechanism that could recover from a crashing child
                child.wait()?;
            } else {
                // Dropping the handle closes it.
                drop(child.stdin.take())
            }
            res
        }
        ReceiverType::UnixSocket(path) => {
            #[cfg(target_os = "linux")]
            let mut unix_stream = if path.starts_with(['.', '/']) {
                UnixStream::connect(path)
            } else {
                use std::os::linux::net::SocketAddrExt;
                let addr = std::os::unix::net::SocketAddr::from_abstract_name(path)?;
                UnixStream::connect_addr(&addr)
            }?;
            #[cfg(not(target_os = "linux"))]
            let mut unix_stream = UnixStream::connect(path)?;
            let res = emit_crashreport(
                &mut unix_stream,
                config,
                config_str,
                metadata_string,
                signum,
            );
            let _ = unix_stream.flush();
            unix_stream
                .shutdown(std::net::Shutdown::Write)
                .context("Could not shutdown writing on the stream")?;
            if config.wait_for_receiver {
                let mut buf = [0; 1];
                // The receiver can signal completion by either writing at least one byte,
                // or by closing the stream.
                let _ = unix_stream.read_exact(&mut buf[..]);
            }
            res
        }
    }
    // Calling "free" in a signal handler is dangerous, so we just leak the
    // objects we took (receiver, metadata, config, etc)
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
        SigHandler::SigAction(handle_posix_sigaction),
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
