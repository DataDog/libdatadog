use std::ffi::{CStr};
use std::ptr;
use libc::{self, Dl_info};

mod crash_handler;
mod intermediate_func;

use crash_handler::install_crash_handler;
use intermediate_func::intermediate_function;


#[repr(C)]
struct UnwContext([u8; 1024]); // Placeholder size for unw_context_t

#[repr(C)]
struct UnwCursor([u8; 1024]); // Placeholder size for unw_cursor_t

extern "C" {
    fn _ULx86_64_init_local(cursor: *mut UnwCursor, context: *mut UnwContext) -> i32;
    fn _ULx86_64_step(cursor: *mut UnwCursor) -> i32;
    fn _ULx86_64_get_proc_name(
        cursor: *mut UnwCursor,
        name: *mut libc::c_char,
        size: usize,
        off: *mut u64,
    ) -> i32;
    fn _ULx86_64_get_reg(cursor: *mut UnwCursor, regnum: i32, val: *mut u64) -> i32;
    fn _Ux86_64_getcontext(context: *mut UnwContext) -> i32;
    fn dladdr(addr: *const libc::c_void, info: *mut Dl_info) -> i32;
}

// Register numbers for instruction pointer and stack pointer
const UNW_REG_IP: i32 = 16;
const UNW_REG_SP: i32 = 17;

fn unwind_stack() {
    unsafe {
        let mut context = UnwContext([0; 1024]);
        let mut cursor = UnwCursor([0; 1024]);

        if _Ux86_64_getcontext(&mut context) != 0 {
            eprintln!("Failed to get context");
            return;
        }

        if _ULx86_64_init_local(&mut cursor, &mut context) != 0 {
            eprintln!("Failed to initialize cursor");
            return;
        }

        let mut name = vec![0 as libc::c_char; 256];
        let mut offset: u64 = 0;
        let mut ip: u64 = 0;
        let mut sp: u64 = 0;

        println!("--- Stack trace ---");

        while _ULx86_64_step(&mut cursor) > 0 {
            _ULx86_64_get_reg(&mut cursor, UNW_REG_IP, &mut ip);
            _ULx86_64_get_reg(&mut cursor, UNW_REG_SP, &mut sp);

            let mut info = Dl_info {
                dli_fname: ptr::null(),
                dli_fbase: ptr::null_mut(),
                dli_sname: ptr::null(),
                dli_saddr: ptr::null_mut(),
            };

            let mut func_name = "<unknown>".to_string();

            if _ULx86_64_get_proc_name(&mut cursor, name.as_mut_ptr(), name.len(), &mut offset) == 0
            {
                func_name = CStr::from_ptr(name.as_ptr()).to_string_lossy().into_owned();
            } else if dladdr(ip as *const libc::c_void, &mut info) != 0 && !info.dli_sname.is_null()
            {
                func_name = CStr::from_ptr(info.dli_sname)
                    .to_string_lossy()
                    .into_owned();
            }

            println!(
                "IP: {:#x}, SP: {:#x}, Function: {}+0x{:x}",
                ip, sp, func_name, offset
            );
        }
        println!("--- End of stack trace ---");
    }
}

// no inlining
#[inline(never)]
fn foo() {
    println!("Inside foo()");
    intermediate_function(); // Call the intermediate function before unwinding
}

fn main() {
    install_crash_handler(unwind_stack);
    println!("Running unwind_c example...");
    foo();
}
