#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(dead_code)]

//! Rust bindings to libunwind
//! 
//! This crate provides raw FFI bindings to libunwind for stack unwinding on Linux.
//! The bindings are automatically generated using bindgen from libunwind.h.
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
//!             if unw_get_reg(&mut cursor, UNW_REG_IP as i32, &mut ip) == 0 {
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

// Include the automatically generated bindings
include!(concat!(env!("OUT_DIR"), "/bindings.rs"));


// Error codes (returned as negative values)
pub const UNW_ESUCCESS: i32 = 0;         // no error
pub const UNW_EUNSPEC: i32 = 1;          // unspecified (general) error
pub const UNW_ENOMEM: i32 = 2;           // out of memory
pub const UNW_EBADREG: i32 = 3;          // bad register number
pub const UNW_EREADONLYREG: i32 = 4;     // attempt to write read-only register
pub const UNW_ESTOPUNWIND: i32 = 5;      // stop unwinding
pub const UNW_EINVALIDIP: i32 = 6;       // invalid IP
pub const UNW_EBADFRAME: i32 = 7;        // bad frame
pub const UNW_EINVAL: i32 = 8;           // unsupported operation or bad value
pub const UNW_EBADVERSION: i32 = 9;      // unwind info has unsupported version
pub const UNW_ENOINFO: i32 = 10;         // no unwind info found

// Register numbers (architecture-specific)

#[cfg(target_arch = "x86_64")]
pub const UNW_X86_64_RIP: i32 = 16;  // Instruction pointer
#[cfg(target_arch = "x86_64")]
pub const UNW_X86_64_RSP: i32 = 7;   // Stack pointer
#[cfg(target_arch = "x86_64")]
pub const UNW_REG_IP: i32 = UNW_X86_64_RIP;  // Alias for IP
#[cfg(target_arch = "x86_64")]
pub const UNW_REG_SP: i32 = UNW_X86_64_RSP;  // Alias for SP

// Add other architectures as needed
#[cfg(target_arch = "x86")]
pub const UNW_REG_IP: i32 = 8;   // EIP on x86
#[cfg(target_arch = "x86")]
pub const UNW_REG_SP: i32 = 4;   // ESP on x86

#[cfg(target_arch = "aarch64")]
pub const UNW_REG_IP: i32 = 32;  // PC on ARM64
#[cfg(target_arch = "aarch64")]
pub const UNW_REG_SP: i32 = 31;  // SP on ARM64

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

// Create architecture-neutral aliases to standard unw_* names
// Each architecture uses different prefixes for the actual symbols

// Architecture-specific function aliases (generated via macro)
include!("lib_aliases.rs");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
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
}
