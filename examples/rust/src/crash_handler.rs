use std::sync::Once;
use libc::{self, c_int, siginfo_t, ucontext_t};

static INIT_SIGNAL_HANDLER: Once = Once::new();
static mut CRASH_UNWIND_FN: Option<fn()> = None;
static mut CRASH_UNWIND_CTX_FN: Option<fn(*mut ucontext_t)> = None;

/// Signal handler for basic unwinding (no context)
extern "C" fn crash_handler(_signum: c_int) {
    eprintln!("Crash detected! Unwinding stack...");
    unsafe {
        if let Some(crash_unwind) = CRASH_UNWIND_FN {
            crash_unwind();
        }
    }
    std::process::exit(1);
}

/// Signal handler that extracts context and forces unwinding from the crash point
extern "C" fn crash_handler_with_context(
    _sig: c_int,
    _info: *mut siginfo_t,
    ucontext: *mut libc::c_void,
) {
    eprintln!("Crash detected! Unwinding stack with context...");
    unsafe {
        if let Some(crash_unwind) = CRASH_UNWIND_CTX_FN {
            if !ucontext.is_null() {
                crash_unwind(ucontext as *mut ucontext_t);
            } else {
                eprintln!("Error: ucontext is null");
            }
        }
    }
    std::process::exit(1);
}

/// Install a basic crash handler (without context)
pub fn install_crash_handler(crash_unwind: fn()) {
    unsafe {
        CRASH_UNWIND_FN = Some(crash_unwind);
    }

    INIT_SIGNAL_HANDLER.call_once(|| unsafe {
        libc::signal(libc::SIGSEGV, crash_handler as usize);
        libc::signal(libc::SIGABRT, crash_handler as usize);
    });
}

/// Install a crash handler that captures context for unwinding
pub fn install_crash_handler_with_context(crash_unwind: fn(*mut ucontext_t)) {
    unsafe {
        CRASH_UNWIND_CTX_FN = Some(crash_unwind);
    }

    INIT_SIGNAL_HANDLER.call_once(|| unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = crash_handler_with_context as usize;
        sa.sa_flags = libc::SA_SIGINFO; // Use SA_SIGINFO to get `ucontext_t`

        libc::sigaction(libc::SIGSEGV, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGABRT, &sa, std::ptr::null_mut());
    });
}
