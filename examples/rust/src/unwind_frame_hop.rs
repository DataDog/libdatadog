extern crate libc;

use libc::{dl_iterate_phdr, dl_phdr_info, size_t, c_void};
use std::ffi::CStr;

extern "C" fn callback(info: *mut dl_phdr_info, _size: size_t, _data: *mut c_void) -> i32 {
    unsafe {
        let name = CStr::from_ptr((*info).dlpi_name).to_string_lossy();
        let base = (*info).dlpi_addr as u64;
        println!("Module: {} @ {:#x}", name, base);
    }
    0
}

fn main() {
    unsafe {
        dl_iterate_phdr(Some(callback), std::ptr::null_mut());
    }
}

