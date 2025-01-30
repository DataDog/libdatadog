use std::sync::Once;
use libc;

static INIT_SIGNAL_HANDLER: Once = Once::new();
static mut CRASH_UNWIND_FN: Option<fn()> = None;

/// Signal handler that unwinds the stack on a crash
extern "C" fn crash_handler(_signum: libc::c_int) {
    eprintln!("Crash detected! Unwinding stack...");
    unsafe {
        if let Some(crash_unwind) = CRASH_UNWIND_FN {
            crash_unwind();
        }
    }
    std::process::exit(1);
}

/// Install a global crash handler
pub fn install_crash_handler(crash_unwind: fn()) {
    unsafe {
        CRASH_UNWIND_FN = Some(crash_unwind);
    }

    INIT_SIGNAL_HANDLER.call_once(|| unsafe {
        libc::signal(libc::SIGSEGV, crash_handler as usize);
        libc::signal(libc::SIGABRT, crash_handler as usize);
    });
}
