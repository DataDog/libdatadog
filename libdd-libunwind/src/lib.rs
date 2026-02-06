// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(target_arch = "x86_64")]
mod libunwind_x86_64;

#[cfg(target_arch = "aarch64")]
mod libunwind_aarch64;

#[cfg(target_arch = "aarch64")]
pub use libunwind_aarch64::*;
#[cfg(target_arch = "x86_64")]
pub use libunwind_x86_64::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore)] // Miri cannot execute FFI calls to libunwind
    fn test_basic_unwind() {
        unsafe {
            let mut context: UnwContext = std::mem::zeroed();
            let mut cursor: UnwCursor = std::mem::zeroed();

            // Get current context
            let ret = unw_getcontext(&mut context);
            assert_eq!(ret, 0, "unw_getcontext failed");

            // Initialize cursor
            let ret = unw_init_local2(&mut cursor, &mut context, 0);
            assert_eq!(ret, 0, "unw_init_local2 failed");

            // Walk the stack
            let mut frames = 0;
            loop {
                let ret = unw_step(&mut cursor);
                if ret <= 0 {
                    break;
                }
                frames += 1;

                // Limit iterations to prevent infinite loops
                if frames > 100 {
                    break;
                }
            }

            // Should have at least a few frames
            assert!(frames > 0, "Expected at least one stack frame");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)] // Miri cannot execute FFI calls to libunwind
    fn test_get_register() {
        unsafe {
            let mut context: UnwContext = std::mem::zeroed();
            let mut cursor: UnwCursor = std::mem::zeroed();

            assert_eq!(unw_getcontext(&mut context), 0);
            assert_eq!(unw_init_local2(&mut cursor, &mut context, 0), 0);

            // Get instruction pointer
            let mut ip: UnwWord = 0;
            let ret = unw_get_reg(&mut cursor, UNW_REG_IP, &mut ip);
            assert_eq!(ret, 0, "Failed to get IP register");
            assert_ne!(ip, 0, "IP should not be zero");

            // Get stack pointer
            let mut sp: UnwWord = 0;
            let ret = unw_get_reg(&mut cursor, UNW_REG_SP, &mut sp);
            assert_eq!(ret, 0, "Failed to get SP register");
            assert_ne!(sp, 0, "SP should not be zero");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)] // Miri cannot execute FFI calls to libunwind
    fn test_backtrace2() {
        unsafe {
            let mut context: UnwContext = std::mem::zeroed();
            assert_eq!(unw_getcontext(&mut context), 0);

            // unw_backtrace2 expects an array of void pointers
            let mut frames: [*mut ::std::os::raw::c_void; 100] = [std::ptr::null_mut(); 100];
            let ret = unw_backtrace2(frames.as_mut_ptr(), 100, &mut context, 0);

            // Return value should be >= 0 (number of frames captured)
            assert!(ret >= 0, "unw_backtrace2 failed with error: {}", ret);

            let frame_count = ret as usize;
            assert!(frame_count > 0, "Expected at least one frame");

            // Print captured frames
            for (i, &frame) in frames.iter().enumerate().take(frame_count) {
                let frame_ptr = frame as usize;
                println!("Frame {}: 0x{:016x}", i, frame_ptr);
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)] // Miri cannot execute FFI calls to libunwind
    fn test_get_proc_name() {
        unsafe {
            let mut context: UnwContext = std::mem::zeroed();
            let mut cursor: UnwCursor = std::mem::zeroed();

            assert_eq!(unw_getcontext(&mut context), 0);
            assert_eq!(
                unw_init_local2(&mut cursor, &mut context, UNW_INIT_LOCAL_ONLY_IP),
                0
            );

            let mut name: [libc::c_char; 100] = [0; 100];
            let ret = unw_get_proc_name(&mut cursor, name.as_mut_ptr(), 100, std::ptr::null_mut());
            assert_eq!(ret, 0, "unw_get_proc_name failed");
            let fn_name = std::ffi::CStr::from_ptr(name.as_ptr()).to_string_lossy();
            assert!(!fn_name.is_empty(), "Name should not be empty");
            // name is managed: _ZN15libdd_libunwind5tests18test_get_proc_name17hec15ec5ad6978a00E
            // we should just chekc that test_get_proc_name is part of it
            assert!(
                fn_name.contains("test_get_proc_name"),
                "Name should contain 'test_get_proc_name'"
            );
        }
    }
}
