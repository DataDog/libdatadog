mod crash_handler;
use crash_handler::install_crash_handler;
use std::ffi::c_void;

extern "C" {
    fn _Unwind_Backtrace(
        trace: extern "C" fn(*mut c_void, *mut c_void) -> u32,
        trace_argument: *mut c_void,
    ) -> u32;
    fn _Unwind_GetIP(ctx: *mut c_void) -> usize;
}

const UNWIND_NO_REASON: u32 = 0;
const UNWIND_FAILURE: u32 = 9;

extern "C" fn trace_fn(
    ctx: *mut c_void,
    arg: *mut c_void,
) -> u32 {
    if arg.is_null() {
        return UNWIND_FAILURE;
    }
    unsafe {
        let callback = &mut *(arg as *mut &mut dyn FnMut(*mut c_void) -> bool);
        if callback(ctx) {
            UNWIND_NO_REASON
        } else {
            UNWIND_FAILURE
        }
    }
}

fn unwind_stack() {
    unsafe {
        println!("Unwinding using _Unwind_Backtrace...");
        let mut callback = |ctx: *mut c_void| {
            let ip = _Unwind_GetIP(ctx);
            println!("IP: {:#x}", ip);
            true
        };

        _Unwind_Backtrace(trace_fn, &mut callback as *mut _ as *mut c_void);
    }
}

fn main() {
    unwind_stack();
    install_crash_handler(unwind_stack);

    println!("Running unwind_libunwind example...");
    unsafe {
        *(std::ptr::null_mut() as *mut i32) = 0; // Trigger a crash
    }
}
