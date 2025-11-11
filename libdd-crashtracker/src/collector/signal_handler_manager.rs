// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use super::crash_handler::handle_posix_sigaction;
use crate::shared::configuration::CrashtrackerConfiguration;
use crate::signal_from_signum;
use libc::{
    c_void, mmap, sigaltstack, siginfo_t, MAP_ANON, MAP_FAILED, MAP_PRIVATE, PROT_NONE, PROT_READ,
    PROT_WRITE, SIGSTKSZ,
};
use libdd_common::unix_utils::terminate;
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler};
use std::ptr;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::SeqCst;

// Linux seems to have the most, supporting up to 64 inclusive
// https://man7.org/linux/man-pages/man7/signal.7.html
const MAX_SIGNALS: usize = 65;
static mut HANDLERS: [Option<(signal::Signal, SigAction)>; MAX_SIGNALS] = [None; MAX_SIGNALS];
static INIT_STARTED: AtomicBool = AtomicBool::new(false);
static INIT_FINISHED: AtomicBool = AtomicBool::new(false);
/// Registers UNIX signal handlers to detect program crashes.
/// This function uses a flag to ensure the initilization only happens once.
/// It is safe (but probably undesirable) to call this function more than once: an error is returned
/// if that happens.
/// PRECONDITIONS:
///     `configure_receiver()` needs to be called before this function.
/// SAFETY:
///     Crash-tracking functions are not guaranteed to be reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     Setting the crash handler itself is not an atomic operation and hence it is possible that a
///     concurrent operation could see partial execution of this function.
///     If a crash occurs during execution of this function, it is possible that
///     the crash handler will have been registered, but the old signal handler
///     will not yet be stored.  This would lead to unexpected behaviour for the
///     user.  This should only matter if something crashes concurrently with
///     this function executing.
///     We currently handle this case by explicitly aborting the process.
pub fn register_crash_handlers(config: &CrashtrackerConfiguration) -> anyhow::Result<()> {
    // Guarantee that the handlers is only mutated once.
    anyhow::ensure!(
        INIT_STARTED
            .compare_exchange(false, true, SeqCst, SeqCst)
            .is_ok(),
        "Attempted to double register crash handlers"
    );

    // Validate signal numbers will fit in the array.
    for signum in config.signals() {
        anyhow::ensure!(*signum >= 0 && *signum < MAX_SIGNALS as i32);
    }

    if config.create_alt_stack() {
        // Safety: This function has no documented preconditions.
        unsafe { create_alt_stack()? };
    }

    let mut errors = vec![];

    for signum in config.signals() {
        let index = *signum as usize;
        // Safety: This function has no documented preconditions.
        match unsafe { register_signal_handler(*signum, config) } {
            // SAFETY:
            // There are only two functions that reference `HANDLERS``.
            // At this point, `INIT_STARTED` is `true` and `INIT_COMPLETED` is false.
            // This function is guarded not to go unless `INIT_STARTED` is false.
            // The other function is guarded not to go unless `INIT_COMPLETED` is true, which only
            // happens at the end of this function.
            // This means that only this instance of this function can access `HANDLERS`
            Ok(handler) => unsafe { HANDLERS[index] = Some(handler) },
            Err(e) => errors.push(format!("Unable to register signal for {signum}: {e:?}")),
        };
    }
    INIT_FINISHED.store(true, SeqCst);
    anyhow::ensure!(
        errors.is_empty(),
        "Errors registering signal handlers {errors:?}"
    );
    Ok(())
}

/// Once we've handled the signal, chain to any previous handlers.
/// SAFETY: This was created by [register_crash_handlers].  There is a tiny
/// instant of time between when the handlers are registered, and the
/// `OLD_HANDLERS` are set.  This should be very short, but is hard to fully
/// eliminate given the existing POSIX APIs.
/// If we run into an unexpected condition we just `_exit` to quit the program without re-raising
/// `SIGABRT`.
pub(crate) unsafe fn chain_signal_handler(
    signum: i32,
    sig_info: *mut siginfo_t,
    ucontext: *mut c_void,
) {
    if !INIT_FINISHED.load(SeqCst) {
        eprintln!("Crashed during signal handler setup, cannot chain {signum}, aborting");
        terminate()
    }
    if signum < 0 || signum >= MAX_SIGNALS as i32 {
        eprintln!("Unexpected value for {signum}, cannot chain, aborting");
        terminate()
    }
    // SAFETY: All accesses to `HANDLERS` are guarded by `INIT_STARTED` and `INIT_FINISHED`.
    // Since `INIT_FINISHED` was guaranteed to be true, we know that no code will ever mutate the
    // static, and hence its safe to read.
    if let Some((signal, sigaction)) = &mut unsafe { HANDLERS[signum as usize] } {
        // How we chain depends on what kind of handler we're chaining to.
        // https://www.gnu.org/software/libc/manual/html_node/Signal-Handling.html
        // https://man7.org/linux/man-pages/man2/sigaction.2.html
        // Follow the approach here:
        // https://stackoverflow.com/questions/6015498/executing-default-signal-handler
        match sigaction.handler() {
            SigHandler::SigDfl => {
                // In the case of a default handler, we want to invoke it so that
                // the core-dump can be generated.  Restoring the handler then
                // re-raising the signal accomplishes that.
                unsafe { signal::sigaction(*signal, sigaction) }.unwrap_or_else(|_| terminate());
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
        }
    } else {
        eprintln!("Missing chain handler for {signum}, cannot chain, aborting");
        terminate()
    }
}

/// Allocates a signal altstack, and puts a guard page at the end.
/// Inspired by https://github.com/rust-lang/rust/pull/69969/files
unsafe fn create_alt_stack() -> anyhow::Result<()> {
    // Ensure that the altstack size is the greater of 16 pages or SIGSTKSZ. This is necessary
    // because the default SIGSTKSZ is 8KB, which we're starting to run into. This new size is
    // arbitrary, but at least it's large enough for our purposes, and yet a small enough part of
    // the process RSS that it shouldn't be a problem.
    let page_size = page_size::get();
    let sigalstack_base_size = std::cmp::max(SIGSTKSZ, 16 * page_size);
    let stackp = mmap(
        ptr::null_mut(),
        sigalstack_base_size + page_size,
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
        ss_size: sigalstack_base_size,
    };
    let rval = sigaltstack(&stack, ptr::null_mut());
    anyhow::ensure!(rval == 0, "sigaltstack failed {rval}");
    Ok(())
}

unsafe fn register_signal_handler(
    signum: i32,
    config: &CrashtrackerConfiguration,
) -> anyhow::Result<(signal::Signal, SigAction)> {
    let signal_type = signal_from_signum(signum)?;

    // Between this and `create_alt_stack()`, there are a few things going on.
    // - It is generally preferable to run in an altstack, given the choice.
    // - Crashtracking does not currently provide any particular guarantees around stack usage; in
    //   fact, it has been observed to frequently exceed 8192 bytes (default SIGSTKSZ) in practice.
    // - Some runtimes (Ruby) will set the altstack to a respectable size (~16k), but will check the
    //   value of the SP during their chained handler and become upset if the altstack is not what
    //   they expect--in these cases, it is necessary to USE the altstack without creating it.
    // - Some runtimes (Python, Rust) will set the altstack to the default size (8k), but will not
    //   check the value of the SP during their chained handler--in these cases, for correct
    //   operation it is necessary to CREATE and USE the altstack.
    // - There are no known cases where it is useful to crate but not use the altstack--this case
    //   handled in `new()` for CrashtrackerConfiguration.
    let extra_saflags = if config.use_alt_stack() {
        SaFlags::SA_ONSTACK
    } else {
        SaFlags::empty()
    };

    let sig_action = SigAction::new(
        SigHandler::SigAction(handle_posix_sigaction),
        SaFlags::SA_NODEFER | extra_saflags,
        signal::SigSet::empty(),
    );

    let old_handler = signal::sigaction(signal_type, &sig_action)?;
    Ok((signal_type, old_handler))
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    #[test]
    fn test_max_signals() {
        assert!(super::MAX_SIGNALS as libc::c_int > libc::SIGRTMAX());
    }
}
