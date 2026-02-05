#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(dead_code)]

//! Rust bindings to libunwind
//!
//! This crate provides raw FFI bindings to libunwind for stack unwinding on Linux.
//! The bindings are manually defined to avoid bindgen dependencies.
//!
//! # Usage in other crates
//!
//! Add this to your Cargo.toml:
//! ```toml
//! [dependencies]
//! libdd-libunwind = { path = "../libdd-libunwind" }
//! ```
//!
//! ## Using from Rust
//!
//! ```rust,ignore
//! use libdd_libunwind::*;
//!
//! fn capture_backtrace() -> Vec<usize> {
//!     let mut frames = Vec::new();
//!     
//!     unsafe {
//!         let mut context: unw_context_t = std::mem::zeroed();
//!         let mut cursor: unw_cursor_t = std::mem::zeroed();
//!         
//!         if unw_getcontext(&mut context) != 0 {
//!             return frames;
//!         }
//!         
//!         if unw_init_local(&mut cursor, &mut context) != 0 {
//!             return frames;
//!         }
//!         
//!         loop {
//!             let mut ip = 0;
//!             if unw_get_reg(&mut cursor, UNW_REG_IP, &mut ip) == 0 {
//!                 frames.push(ip as usize);
//!             }
//!             
//!             if unw_step(&mut cursor) <= 0 {
//!                 break;
//!             }
//!         }
//!     }
//!     
//!     frames
//! }
//! ```
//!
//! ## Using from C code via build.rs
//!
//! ```rust,ignore
//! fn main() {
//!     // Get paths exported by libdd-libunwind
//!     if let Ok(include_path) = std::env::var("DEP_UNWIND_INCLUDE") {
//!         println!("cargo:include={}", include_path);
//!         cc::Build::new()
//!             .include(&include_path)
//!             .file("src/my_code.c")
//!             .compile("my_code");
//!     }
//! }
//! ```

// Manual type definitions - no bindgen needed!
// These are the essential types from libunwind headers

// ============================================================================
// Basic types
// ============================================================================

pub type unw_word_t = u64;
pub type unw_sword_t = i64;

// Architecture-specific cursor size
#[cfg(target_arch = "x86_64")]
pub const UNW_TDEP_CURSOR_LEN: usize = 127;

#[cfg(target_arch = "aarch64")]
pub const UNW_TDEP_CURSOR_LEN: usize = 4096; // ARM64 cursor size

#[cfg(target_arch = "x86")]
pub const UNW_TDEP_CURSOR_LEN: usize = 127;

// Opaque cursor structure
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct unw_cursor {
    pub opaque: [unw_word_t; UNW_TDEP_CURSOR_LEN],
}

pub type unw_cursor_t = unw_cursor;

impl Default for unw_cursor {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

// Context is platform ucontext_t (from libc)
pub type unw_context_t = libc::ucontext_t;

// Floating point register type
pub type unw_fpreg_t = u128;

// Address space (opaque pointer)
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct unw_addr_space {
    _unused: [u8; 0],
}

pub type unw_addr_space_t = *mut unw_addr_space;

// ============================================================================
// Procedure info structures
// ============================================================================

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct unw_tdep_proc_info_t {
    pub unused: u8,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct unw_proc_info {
    pub start_ip: unw_word_t,
    pub end_ip: unw_word_t,
    pub lsda: unw_word_t,
    pub handler: unw_word_t,
    pub gp: unw_word_t,
    pub flags: unw_word_t,
    pub format: ::std::os::raw::c_int,
    pub unwind_info_size: ::std::os::raw::c_int,
    pub unwind_info: *mut ::std::os::raw::c_void,
    pub extra: unw_tdep_proc_info_t,
}

impl Default for unw_proc_info {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

pub type unw_proc_info_t = unw_proc_info;

// Save location structure
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct unw_save_loc {
    pub type_: ::std::os::raw::c_int,
    pub extra: unw_word_t,
}

impl Default for unw_save_loc {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

pub type unw_save_loc_t = unw_save_loc;

// ============================================================================
// Accessors and callbacks
// ============================================================================

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct unw_accessors_t {
    _unused: [u8; 0],
}

pub type unw_reg_states_callback = ::std::option::Option<
    unsafe extern "C" fn(
        token: *mut ::std::os::raw::c_void,
        reg_states_data: *mut ::std::os::raw::c_void,
        reg_states_data_size: usize,
        arg: *mut ::std::os::raw::c_void,
    ) -> ::std::os::raw::c_int,
>;

pub type unw_iterate_phdr_callback_t = ::std::option::Option<
    unsafe extern "C" fn(
        info: *mut ::std::os::raw::c_void,
        size: usize,
        arg: *mut ::std::os::raw::c_void,
    ) -> ::std::os::raw::c_int,
>;

// ============================================================================
// Constants and enums
// ============================================================================

// Caching policy enum
pub type unw_caching_policy_t = ::std::os::raw::c_uint;
pub const UNW_CACHE_NONE: unw_caching_policy_t = 0;
pub const UNW_CACHE_GLOBAL: unw_caching_policy_t = 1;
pub const UNW_CACHE_PER_THREAD: unw_caching_policy_t = 2;

// Error codes (returned as negative values)
pub const UNW_ESUCCESS: i32 = 0; // no error
pub const UNW_EUNSPEC: i32 = 1; // unspecified (general) error
pub const UNW_ENOMEM: i32 = 2; // out of memory
pub const UNW_EBADREG: i32 = 3; // bad register number
pub const UNW_EREADONLYREG: i32 = 4; // attempt to write read-only register
pub const UNW_ESTOPUNWIND: i32 = 5; // stop unwinding
pub const UNW_EINVALIDIP: i32 = 6; // invalid IP
pub const UNW_EBADFRAME: i32 = 7; // bad frame
pub const UNW_EINVAL: i32 = 8; // unsupported operation or bad value
pub const UNW_EBADVERSION: i32 = 9; // unwind info has unsupported version
pub const UNW_ENOINFO: i32 = 10; // no unwind info found

// Register numbers (architecture-specific)

#[cfg(target_arch = "x86_64")]
pub const UNW_X86_64_RIP: i32 = 16; // Instruction pointer
#[cfg(target_arch = "x86_64")]
pub const UNW_X86_64_RSP: i32 = 7; // Stack pointer
#[cfg(target_arch = "x86_64")]
pub const UNW_REG_IP: i32 = UNW_X86_64_RIP; // Alias for IP
#[cfg(target_arch = "x86_64")]
pub const UNW_REG_SP: i32 = UNW_X86_64_RSP; // Alias for SP

// Add other architectures as needed
#[cfg(target_arch = "x86")]
pub const UNW_REG_IP: i32 = 8; // EIP on x86
#[cfg(target_arch = "x86")]
pub const UNW_REG_SP: i32 = 4; // ESP on x86

#[cfg(target_arch = "aarch64")]
pub const UNW_REG_IP: i32 = 32; // PC on ARM64
#[cfg(target_arch = "aarch64")]
pub const UNW_REG_SP: i32 = 31; // SP on ARM64

// Helper function to convert error code to string
pub fn error_string(err: i32) -> &'static str {
    match err {
        0 => "UNW_ESUCCESS: no error",
        -1 => "UNW_EUNSPEC: unspecified (general) error",
        -2 => "UNW_ENOMEM: out of memory",
        -3 => "UNW_EBADREG: bad register number",
        -4 => "UNW_EREADONLYREG: attempt to write read-only register",
        -5 => "UNW_ESTOPUNWIND: stop unwinding",
        -6 => "UNW_EINVALIDIP: invalid IP",
        -7 => "UNW_EBADFRAME: bad frame",
        -8 => "UNW_EINVAL: unsupported operation or bad value",
        -9 => "UNW_EBADVERSION: unwind info has unsupported version",
        -10 => "UNW_ENOINFO: no unwind info found",
        _ => "Unknown error code",
    }
}

// ============================================================================
// External function declarations and aliases
// ============================================================================
//
// libunwind uses architecture-specific function names like _ULx86_64_init_local.
// This macro both declares the extern function AND creates a standard unw_* alias.

macro_rules! unw_functions {
    ($arch:ident) => {
        paste::paste! {
            extern "C" {
                // Generic functions (no "L" prefix in arch name)
                pub fn [<_ U $arch _getcontext>](context: *mut unw_context_t) -> ::std::os::raw::c_int;
                pub fn [<_ U $arch _strerror>](err: ::std::os::raw::c_int) -> *const ::std::os::raw::c_char;
                pub fn [<_ U $arch _regname>](reg: ::std::os::raw::c_int) -> *const ::std::os::raw::c_char;

                // Local unwinding functions ("L" in arch prefix)
                pub fn [<_ UL $arch _init_local>](cursor: *mut unw_cursor_t, context: *mut unw_context_t) -> ::std::os::raw::c_int;
                pub fn [<_ UL $arch _init_local2>](cursor: *mut unw_cursor_t, context: *mut unw_context_t, flag: ::std::os::raw::c_int) -> ::std::os::raw::c_int;
                pub fn [<_ UL $arch _step>](cursor: *mut unw_cursor_t) -> ::std::os::raw::c_int;
                pub fn [<_ UL $arch _get_reg>](cursor: *mut unw_cursor_t, reg: ::std::os::raw::c_int, valp: *mut unw_word_t) -> ::std::os::raw::c_int;
                pub fn [<_ UL $arch _get_proc_name>](cursor: *mut unw_cursor_t, buffer: *mut ::std::os::raw::c_char, len: usize, offset: *mut unw_word_t) -> ::std::os::raw::c_int;
                pub fn [<_ UL $arch _get_proc_info>](cursor: *mut unw_cursor_t, pip: *mut unw_proc_info_t) -> ::std::os::raw::c_int;
            }

            // Create public aliases with standard unw_* names
            pub use {
                [<_ U $arch _getcontext>] as unw_getcontext,
                [<_ U $arch _strerror>] as unw_strerror,
                [<_ U $arch _regname>] as unw_regname,
                [<_ UL $arch _init_local>] as unw_init_local,
                [<_ UL $arch _init_local2>] as unw_init_local2,
                [<_ UL $arch _step>] as unw_step,
                [<_ UL $arch _get_reg>] as unw_get_reg,
                [<_ UL $arch _get_proc_name>] as unw_get_proc_name,
                [<_ UL $arch _get_proc_info>] as unw_get_proc_info,
            };
        }
    };
}

extern "C" {
    pub fn unw_backtrace2(
        frames: *mut *mut ::std::os::raw::c_void,
        max_frames: ::std::os::raw::c_int,
        context: *mut unw_context_t,
        inner_frame_enum: ::std::os::raw::c_int,
    ) -> ::std::os::raw::c_int;
}

// Invoke for each supported architecture
#[cfg(target_arch = "x86_64")]
unw_functions!(x86_64);

#[cfg(target_arch = "aarch64")]
unw_functions!(aarch64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore)]  // Miri cannot execute FFI calls to libunwind
    fn test_basic_unwind() {
        unsafe {
            let mut context: unw_context_t = std::mem::zeroed();
            let mut cursor: unw_cursor_t = std::mem::zeroed();

            // Get current context
            let ret = unw_getcontext(&mut context);
            assert_eq!(ret, 0, "unw_getcontext failed");

            // Initialize cursor
            let ret = unw_init_local(&mut cursor, &mut context);
            assert_eq!(ret, 0, "unw_init_local failed");

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
    #[cfg_attr(miri, ignore)]  // Miri cannot execute FFI calls to libunwind
    fn test_get_register() {
        unsafe {
            let mut context: unw_context_t = std::mem::zeroed();
            let mut cursor: unw_cursor_t = std::mem::zeroed();

            assert_eq!(unw_getcontext(&mut context), 0);
            assert_eq!(unw_init_local(&mut cursor, &mut context), 0);

            // Get instruction pointer
            let mut ip: unw_word_t = 0;
            let ret = unw_get_reg(&mut cursor, UNW_REG_IP, &mut ip);
            assert_eq!(ret, 0, "Failed to get IP register");
            assert_ne!(ip, 0, "IP should not be zero");

            // Get stack pointer
            let mut sp: unw_word_t = 0;
            let ret = unw_get_reg(&mut cursor, UNW_REG_SP, &mut sp);
            assert_eq!(ret, 0, "Failed to get SP register");
            assert_ne!(sp, 0, "SP should not be zero");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]  // Miri cannot execute FFI calls to libunwind
    fn test_backtrace2() {
        unsafe {
            let mut context: unw_context_t = std::mem::zeroed();
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
}
