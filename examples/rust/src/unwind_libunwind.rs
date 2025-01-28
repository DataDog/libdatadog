mod crash_handler;
use crash_handler::install_crash_handler;
use std::ffi::c_void;

#[repr(u32)]
pub enum _Unwind_Reason_Code {
    _URC_NO_REASON = 0,
    _URC_FOREIGN_EXCEPTION_CAUGHT = 1,
    _URC_FATAL_PHASE2_ERROR = 2,
    _URC_FATAL_PHASE1_ERROR = 3,
    _URC_NORMAL_STOP = 4,
    _URC_END_OF_STACK = 5,
    _URC_HANDLER_FOUND = 6,
    _URC_INSTALL_CONTEXT = 7,
    _URC_CONTINUE_UNWIND = 8,
    _URC_FAILURE = 9, // Used only by ARM EABI
}

unsafe extern "C" fn trace_fn(
    ctx: *mut libunwind::_Unwind_Context,
    arg: *mut c_void,
) -> u32 {
    if arg.is_null() {
        return _Unwind_Reason_Code::_URC_FAILURE as u32;
    }

    let callback = &mut *(arg as *mut &mut dyn FnMut(*mut libunwind::_Unwind_Context) -> bool);
    if callback(ctx) {
        _Unwind_Reason_Code::_URC_NO_REASON as u32
    } else {
        _Unwind_Reason_Code::_URC_FAILURE as u32
    }
}

fn unwind_stack() {
    unsafe {
        println!("Unwinding using libunwind...");
        let mut callback = |ctx: *mut libunwind::_Unwind_Context| {
            let ip = libunwind::_Unwind_GetIP(ctx) as *mut c_void;
            println!("IP: {:#x}", ip as usize);
            true
        };

        libunwind::_Unwind_Backtrace(Some(trace_fn), &mut callback as *mut _ as *mut c_void);
    }
}

fn main() {
    install_crash_handler(unwind_stack);

    println!("Running unwind_libunwind example...");
    unsafe {
        *(std::ptr::null_mut() as *mut i32) = 0; // Trigger a crash
    }
}
