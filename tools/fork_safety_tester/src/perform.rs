use std::{ffi::CStr, thread};

use nix::libc::RTLD_LAZY;


pub fn perform_allocation_rust() {
    let expected_size = 1 * 1024 * 1024;
    let mut buffer = vec![0_u8; expected_size];
    buffer[0] = 0xff;
    buffer[expected_size - 1] = 0xff;
    assert_eq!(buffer.len(), expected_size);
}

pub fn perform_allocation() {
    unsafe {
        let res = nix::libc::malloc(1*1024);
        nix::libc::free(res);
    }
}

pub fn spawn_continous_action<F: Fn() + Send + 'static>(
    name: &str,
    id: usize,
    status_every: usize,
    f: F,
) {
    let name = format!("{name}[{id}]");
    thread::spawn(move || {
        let mut iterations = 1;
        loop {
            f();
            iterations += 1;
            if iterations % status_every == 0 {
                // eprintln!("{}: {}", name, iterations);
            }
        }
    });
}

pub fn perform_dlopen() {
    unsafe {
        let handle = nix::libc::dlopen(
            CStr::from_bytes_with_nul_unchecked(b"libsystemd.so.0\0").as_ptr(),
            RTLD_LAZY,
        );
        if handle.is_null() {
            panic!("failed loading systemd.so")
        }
        if nix::libc::dlclose(handle) != 0 {
            panic!("failed closing handle")
        }
    }
}