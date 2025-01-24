use std::ffi::CStr;
use std::panic;
use std::sync::Once;
use backtrace::{resolve_frame_unsynchronized, trace_unsynchronized};


#[repr(C)]
struct UnwContext([u8; 1024]); // Placeholder size for unw_context_t

#[repr(C)]
struct UnwCursor([u8; 1024]); // Placeholder size for unw_cursor_t

// these are the symbols exposed in x86_64
// there are macros in the C headers that we do not use
extern "C" {
    fn _ULx86_64_init_local(cursor: *mut UnwCursor, context: *mut UnwContext) -> i32;
    fn _ULx86_64_step(cursor: *mut UnwCursor) -> i32;
    fn _ULx86_64_get_proc_name(cursor: *mut UnwCursor, name: *mut libc::c_char, size: usize, off: *mut u64) -> i32;
    fn _ULx86_64_get_reg(cursor: *mut UnwCursor, regnum: i32, val: *mut u64) -> i32;
    fn _Ux86_64_getcontext(context: *mut UnwContext) -> i32;
}

macro_rules! unw_init_local {
    ($cursor:expr, $context:expr) => {
        unsafe { _ULx86_64_init_local($cursor, $context) }
    };
}

macro_rules! unw_step {
    ($cursor:expr) => {
        unsafe { _ULx86_64_step($cursor) }
    };
}

macro_rules! unw_get_proc_name {
    ($cursor:expr, $name:expr, $size:expr, $offset:expr) => {
        unsafe { _ULx86_64_get_proc_name($cursor, $name, $size, $offset) }
    };
}

macro_rules! unw_get_reg {
    ($cursor:expr, $regnum:expr, $val:expr) => {
        unsafe { _ULx86_64_get_reg($cursor, $regnum, $val) }
    };
}

// todo: adjust to arm64
const UNW_REG_IP: i32 = 16; // Instruction pointer register
const UNW_REG_SP: i32 = 17; // Stack pointer register

fn unwind_stack() -> Vec<String> {
    let mut frames = vec![];

    unsafe {
        let mut context = UnwContext([0; 1024]);
        let mut cursor = UnwCursor([0; 1024]);

        if _Ux86_64_getcontext(&mut context) != 0 {
            eprintln!("Failed to get context");
            return frames;
        }

        if unw_init_local!(&mut cursor, &mut context) != 0 {
            eprintln!("Failed to initialize cursor");
            return frames;
        }

        let mut ip: u64 = 0;
        let mut sp: u64 = 0;
        let mut name = vec![0 as libc::c_char; 256];
        let mut offset: u64 = 0;

        while unw_step!(&mut cursor) > 0 {
            unw_get_reg!(&mut cursor, UNW_REG_IP, &mut ip);
            unw_get_reg!(&mut cursor, UNW_REG_SP, &mut sp);
            // todo: we do not need the symbols here (we can use blazesym)
            if unw_get_proc_name!(&mut cursor, name.as_mut_ptr(), name.len(), &mut offset) == 0 {
                let func_name = CStr::from_ptr(name.as_ptr())
                    .to_string_lossy()
                    .into_owned();
                frames.push(format!(
                    "IP: {:#x}, SP: {:#x}, Function: {}+{:#x}",
                    ip, sp, func_name, offset
                ));
            } else {
                frames.push(format!(
                    "IP: {:#x}, SP: {:#x}, Function: <unknown>",
                    ip, sp
                ));
            }
        }
    }

    frames
}

fn unwind_with_backtrace() {
    unsafe {
        println!("Starting backtrace-rs unwinding...");

        trace_unsynchronized(|frame| {
            backtrace::resolve_frame_unsynchronized(frame, |symbol| {
                let mut info = String::new();
                if let Some(name) = symbol.name() {
                    info.push_str(&format!("Function: {}", name));
                }
                if let Some(file) = symbol.filename() {
                    info.push_str(&format!(", File: {:?}", file));
                }
                if let Some(line) = symbol.lineno() {
                    info.push_str(&format!(", Line: {}", line));
                }
                println!("Frame: IP: {:?}, {}", frame.ip(), info);
            });
            true // Continue tracing
        });
    }
}


// link to gcc_s does not work for some reason, workaround:
// LD_PRELOAD=/usr/lib/libgcc_s.so ./target/debug/unwind_example
// This should give this result
// IP: 0x56074b8747cd, SP: 0x7ffe808cb0e0, Function: _ZN14unwind_example4main17hb317b7d12fbefac5E+0x2d
// IP: 0x56074b8752db, SP: 0x7ffe808cb230, Function: _ZN4core3ops8function6FnOnce9call_once17h75cf2982c08e97f8E+0xb
// IP: 0x56074b875a5e, SP: 0x7ffe808cb250, Function: _ZN3std3sys9backtrace28__rust_begin_short_backtrace17h4d254773be70884fE+0xe
// IP: 0x56074b874eb1, SP: 0x7ffe808cb270, Function: _ZN3std2rt10lang_start28_$u7b$$u7b$closure$u7d$$u7d$17h616289c43a9bcadaE+0x11
// IP: 0x56074b890df8, SP: 0x7ffe808cb290, Function: _ZN3std2rt19lang_start_internal17h575d491f6f79b393E+0x508
// IP: 0x56074b874e8a, SP: 0x7ffe808cb3d0, Function: _ZN3std2rt10lang_start17hfd9dcfe3b33226c4E+0x3a
// IP: 0x56074b8749ce, SP: 0x7ffe808cb410, Function: main+0x1e
// IP: 0x7ab89ffa0496, SP: 0x7ffe808cb420, Function: <unknown>
// IP: 0x0, SP: 0x7ffe808cb430, Function: <unknown>
// IP: 0x7ffe808cbec8, SP: 0x7ffe808cb438, Function: <unknown>
// IP: 0x752f67756265642f, SP: 0x7ffe808cb448, Function: <unknown>

static INIT_SIGNAL_HANDLER: Once = Once::new();

extern "C" fn crash_handler(_signum: libc::c_int) {
    eprintln!("Crash detected! Unwinding stack:");
    let frames = unwind_stack();
    for frame in frames {
        eprintln!("{}", frame);
    }

    unwind_with_backtrace();
    std::process::exit(1);
}

fn install_crash_handler() {
    INIT_SIGNAL_HANDLER.call_once(|| unsafe {
        libc::signal(libc::SIGSEGV, crash_handler as usize);
        libc::signal(libc::SIGABRT, crash_handler as usize);
    });
}

fn main() {
    install_crash_handler();

    println!("Starting stack unwinding...");
    let frames = unwind_stack();
    for frame in frames {
        println!("{}", frame);
    }

    println!("Generating a crash...");
    unsafe {
        *(std::ptr::null_mut() as *mut i32) = 0; // Dereference a null pointer to generate a crash
    }
}
