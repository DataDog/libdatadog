use std::{
    ffi::{self, CStr, CString},
    sync::mpsc::Iter,
};

use ddcommon::cstr;
use ddtelemetry::ipc::sidecar::start_or_connect_to_sidecar;
use nix::libc;
use spawn_worker::ExecVec;

use crate::sidecar::maybe_start;

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

pub unsafe fn environ() -> *mut *const *const ffi::c_char {
    extern "C" {
        static mut environ: *const *const ffi::c_char;
    }
    std::ptr::addr_of_mut!(environ)
}

pub struct CListMutPtr<'a>{
    inner: &'a mut [*const ffi::c_char],
    elements: usize
}

impl<'a> CListMutPtr<'a> {
    pub unsafe fn from_raw_parts(ptr: *mut *const ffi::c_char) -> Self {
        let mut len = 0;
        while *ptr.add(len) != std::ptr::null() {
            len += 1;
        }
        Self{
            inner: std::slice::from_raw_parts_mut(ptr, len+1),
            elements: len,
        }
    }

    pub unsafe fn as_ptr(&self) -> *const *const ffi::c_char {
        self.inner.as_ptr()
    }

    pub unsafe fn to_cstr_vec(&self) -> Vec<&CStr> {
        self.inner[0..self.elements].iter().map(|s| CStr::from_ptr(*s)).collect()
    }

    /// remove entry from a slice, shifting other entries in its place
    pub unsafe fn remove_entry<F: Fn(&[u8]) -> bool>(&mut self, predicate: F) -> Option<*const ffi::c_char> {
        for i in (0..self.elements).rev() {
            let elem = CStr::from_ptr(self.inner[i]);
            if predicate(elem.to_bytes()) {
                for src in i+1..self.elements {
                    self.inner[src-1] = self.inner[src]
                }
                self.elements -= 1;
                return Some(elem.as_ptr());
            }
        }

        None
    }

    pub unsafe fn into_exec_vec(self) -> ExecVec {
        let mut vec = ExecVec::empty(); //TODO: this needs some deuglification to prevent duped envs etc
        for item in &self.inner[0..self.elements] {
            vec.push_ptr(*item);
        }

        vec
    }
}


unsafe extern "C" fn new_main(
    argc: ffi::c_int,
    argv: *const *const ffi::c_char,
    _envp: *const *const ffi::c_char,
) -> ffi::c_int {
    let mut env = CListMutPtr::from_raw_parts(*environ() as *mut *const ffi::c_char);
    env.remove_entry(|e| e.starts_with("LD_PRELOAD=".as_bytes()));
    
    let mut env = env.into_exec_vec();
    let path = maybe_start().unwrap();
    env.push(CString::new(format!("DD_TRACE_AGENT_URL=unix://{}", path.to_string_lossy())).unwrap());


    let old_environ = *environ(); 
    *environ() = env.as_ptr();

    let rv = match unsafe { ORIGINAL_MAIN } {
        Some(f) => f(argc, argv, env.as_ptr()),
        None => 0,
    };

    // setting back before exiting as env will be garbage collected and all of its references will become invalid
    *environ() = old_environ;
    rv
}

unsafe fn dlsym_fn(handle: *mut ffi::c_void, str: &CStr) -> Option<*mut ffi::c_void> {
    let addr = libc::dlsym(handle, str.as_ptr());
    if addr.is_null() {
        return None;
    }

    Some(std::mem::transmute(addr))
}

#[cfg(not(test))]
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
    let libc_start_main = unsafe {
        std::mem::transmute::<_, StartMainFn>(
            dlsym_fn(libc::RTLD_NEXT, cstr!("__libc_start_main")).unwrap(),
        )
    } as StartMainFn;
    unsafe { ORIGINAL_MAIN = Some(main) };
    libc_start_main(new_main, argc, argv, init, fini, rtld_fini, stack_end);
}

static mut ORIGINAL_MAIN: Option<MainFn> = None;
