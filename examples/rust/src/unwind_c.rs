use std::ffi::CStr;
use libc;
mod crash_handler;
use crash_handler::install_crash_handler;

#[repr(C)]
struct UnwContext([u8; 1024]); // Placeholder size for unw_context_t

#[repr(C)]
struct UnwCursor([u8; 1024]); // Placeholder size for unw_cursor_t

extern "C" {
    fn _ULx86_64_init_local(cursor: *mut UnwCursor, context: *mut UnwContext) -> i32;
    fn _ULx86_64_step(cursor: *mut UnwCursor) -> i32;
    fn _ULx86_64_get_proc_name(cursor: *mut UnwCursor, name: *mut libc::c_char, size: usize, off: *mut u64) -> i32;
    fn _Ux86_64_getcontext(context: *mut UnwContext) -> i32;
}

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

        while _ULx86_64_step(&mut cursor) > 0 {
            if _ULx86_64_get_proc_name(&mut cursor, name.as_mut_ptr(), name.len(), &mut offset) == 0 {
                let func_name = CStr::from_ptr(name.as_ptr()).to_string_lossy().into_owned();
                println!("Function: {}+0x{:x}", func_name, offset);
            } else {
                println!("Function: <unknown>");
            }
        }
    }
}

fn main() {
    install_crash_handler(unwind_stack);

    println!("Running unwind_c example...");
    unsafe {
        *(std::ptr::null_mut() as *mut i32) = 0; // Trigger a crash
    }
}
