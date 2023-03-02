use std::ffi::{self, CStr, CString};

use ddcommon::cstr;
use nix::libc;
use smallvec::SmallVec;
use spawn_worker::utils::{raw_env, CListMutPtr, ExecVec};

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

#[allow(dead_code)]
unsafe extern "C" fn new_main(
    argc: ffi::c_int,
    argv: *const *const ffi::c_char,
    _envp: *const *const ffi::c_char,
) -> ffi::c_int {
    let mut env = raw_env::as_clist();
    let path = maybe_start().unwrap();
    env.remove_entry(|e| e.starts_with("LD_PRELOAD=".as_bytes()));

    let mut env: ExecVec<10> = env.into_exec_vec();

    env.push_cstring(
        CString::new(format!(
            "DD_TRACE_AGENT_URL=unix://{}",
            path.to_string_lossy()
        ))
        .expect("extra null found in in new env variable"),
    );

    let old_environ = raw_env::swap(env.as_ptr());

    let rv = match unsafe { ORIGINAL_MAIN } {
        Some(main) => main(argc, argv, env.as_ptr()),
        None => 0,
    };

    // setting back before exiting as env will be garbage collected and all of its references will become invalid
    raw_env::swap(old_environ);
    rv
}

#[no_mangle]
pub unsafe extern "C" fn __libc_start_main(
    main: MainFn,
    argc: ffi::c_int,
    argv: *const *const ffi::c_char,
    init: InitFn,
    fini: FiniFn,
    rtld_fini: FiniFn,
    stack_end: *const ffi::c_void,
) {
    let libc_start_main =
        spawn_worker::utils::dlsym::<StartMainFn>(libc::RTLD_NEXT, cstr!("__libc_start_main"))
            .unwrap();
    ORIGINAL_MAIN = Some(main);
    #[cfg(not(test))]
    libc_start_main(new_main, argc, argv, init, fini, rtld_fini, stack_end);
    #[cfg(test)]
    libc_start_main(
        unsafe { ORIGINAL_MAIN.unwrap() },
        argc,
        argv,
        init,
        fini,
        rtld_fini,
        stack_end,
    );
}

static mut ORIGINAL_MAIN: Option<MainFn> = None;
