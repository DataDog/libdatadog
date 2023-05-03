// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::{
    ffi::{self, CStr},
    process::Command,
};

use ddcommon::cstr;
use nix::libc;

type StartMainFn = extern "C" fn(
    main: MainFn,
    argc: ffi::c_int,
    argv: *const *const ffi::c_char,
    init: InitFn,
    fini: FiniFn,
    rtld_fini: FiniFn,
    stack_end: *const ffi::c_void,
);
type MainFn = unsafe extern "C" fn(
    ffi::c_int,
    *const *const ffi::c_char,
    *const *const ffi::c_char,
) -> ffi::c_int;
type InitFn = extern "C" fn(ffi::c_int, *const *const ffi::c_char, *const *const ffi::c_char);
type FiniFn = extern "C" fn();

/// # Safety
///
/// caller must ensure its safe to read `environ` global value
pub unsafe fn environ() -> *mut *const *const ffi::c_char {
    extern "C" {
        static mut environ: *const *const ffi::c_char;
    }
    std::ptr::addr_of_mut!(environ)
}

pub struct CListMutPtr<'a> {
    inner: &'a mut [*const ffi::c_char],
    elements: usize,
}

impl<'a> CListMutPtr<'a> {
    /// # Safety
    ///
    /// pointers passed to this method must remain valid for the lifetime of CListMutPtr object
    pub unsafe fn from_raw_parts(ptr: *mut *const ffi::c_char) -> Self {
        let mut len = 0;
        while !(*ptr.add(len)).is_null() {
            len += 1;
        }
        Self {
            inner: std::slice::from_raw_parts_mut(ptr, len + 1),
            elements: len,
        }
    }

    pub fn as_ptr(&self) -> *const *const ffi::c_char {
        self.inner.as_ptr()
    }

    /// # Safety
    /// entries in self.inner must be valid null terminated c strings
    pub unsafe fn to_cstr_vec(&self) -> Vec<&CStr> {
        self.inner[0..self.elements]
            .iter()
            .map(|s| CStr::from_ptr(*s))
            .collect()
    }

    /// remove entry from a slice, shifting other entries in its place
    ///
    /// # Safety
    /// entries in self.inner must be valid null terminated c strings
    pub unsafe fn remove_entry<F: Fn(&[u8]) -> bool>(
        &mut self,
        predicate: F,
    ) -> Option<*const ffi::c_char> {
        for i in (0..self.elements).rev() {
            let elem = CStr::from_ptr(self.inner[i]);
            if predicate(elem.to_bytes()) {
                for src in i + 1..self.elements {
                    self.inner[src - 1] = self.inner[src]
                }
                self.elements -= 1;
                return Some(elem.as_ptr());
            }
        }

        None
    }
}

#[allow(dead_code)]
unsafe extern "C" fn new_main(
    argc: ffi::c_int,
    argv: *const *const ffi::c_char,
    _envp: *const *const ffi::c_char,
) -> ffi::c_int {
    println!("Sending stdout and stderr to Datadog");
    let metrics_agent_cmd = "./metrics_agent";

    Command::new(metrics_agent_cmd)
        .spawn()
        .expect("Failed to run metrics agent");

    match unsafe { ORIGINAL_MAIN } {
        Some(f) => f(argc, argv, _envp),
        None => 0,
    }
}

unsafe fn dlsym_fn(handle: *mut ffi::c_void, str: &CStr) -> Option<*mut ffi::c_void> {
    let addr = libc::dlsym(handle, str.as_ptr());
    if addr.is_null() {
        return None;
    }

    Some(addr)
}

#[no_mangle]
pub extern "C" fn __libc_start_main(
    main: MainFn,
    argc: ffi::c_int,
    argv: *const *const ffi::c_char,
    init: InitFn,
    fini: FiniFn,
    rtld_fini: FiniFn,
    stack_end: *const ffi::c_void,
) {
    unsafe {
        let libc_start_main = std::mem::transmute::<_, StartMainFn>(
            dlsym_fn(libc::RTLD_NEXT, cstr!("__libc_start_main")).unwrap(),
        ) as StartMainFn;

        ORIGINAL_MAIN = Some(main);

        if std::process::id() == 1 || std::process::id() == 7 {
            println!("Skipping process ID 1 or 7");
            libc_start_main(
                ORIGINAL_MAIN.unwrap(),
                argc,
                argv,
                init,
                fini,
                rtld_fini,
                stack_end,
            )
        }
        // the pointer to envp is the next integer after argv
        // it's a null-terminated array of strings
        // Note: for some reason setting a new env in new_main didn't work,
        // as the subprocesses spawned by this process still contain LD_PRELOAD,
        // but removing it here does indeed work
        let envp_ptr = argv.offset(argc as isize + 1) as *mut *const ffi::c_char;
        let mut env_vec = CListMutPtr::from_raw_parts(envp_ptr);
        match env_vec.remove_entry(|e| e.starts_with("LD_PRELOAD=".as_bytes())) {
            Some(preload_lib) => {
                println!(
                    "Found {} in process {}, starting bootstrap process",
                    CStr::from_ptr(preload_lib as *const ffi::c_char)
                        .to_str()
                        .expect("Couldn't convert LD_PRELOAD lib to string"),
                    std::process::id(),
                );

                libc_start_main(new_main, argc, argv, init, fini, rtld_fini, stack_end)
            }
            None => {
                println!(
                    "No LD_PRELOAD found in env of process {}",
                    std::process::id()
                );
                libc_start_main(
                    ORIGINAL_MAIN.unwrap(),
                    argc,
                    argv,
                    init,
                    fini,
                    rtld_fini,
                    stack_end,
                )
            }
        }
    }
}

static mut ORIGINAL_MAIN: Option<MainFn> = None;
