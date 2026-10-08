// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(all(target_os = "linux", target_pointer_width = "64", not(miri)))]

use std::cell::Cell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use libc::{PROT_READ, PROT_WRITE, dl_phdr_info};
use libdd_gotter::{DynamicInfo, hook_symbol_excluding_self, iterate_libraries, read_proc_maps};

type Callback = Option<unsafe extern "C" fn(*mut dl_phdr_info, usize, *mut c_void) -> i32>;
type Iterate = unsafe extern "C" fn(Callback, *mut c_void) -> i32;

thread_local! {
    static LOAD_PATH: Cell<*const c_char> = const { Cell::new(std::ptr::null()) };
    static WALKS: Cell<usize> = const { Cell::new(0) };
    static LOADED: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
}

// Load between symbol lookup and the patch walk, without changing permissions.
#[unsafe(no_mangle)]
unsafe extern "C" fn dl_iterate_phdr(callback: Callback, data: *mut c_void) -> i32 {
    unsafe {
        let next = libc::dlsym(libc::RTLD_NEXT, c"dl_iterate_phdr".as_ptr());
        if next.is_null() {
            libc::abort();
        }
        let next: Iterate = std::mem::transmute(next);
        if !LOAD_PATH.with(Cell::get).is_null() {
            let walk = WALKS.with(|walks| {
                let next = walks.get() + 1;
                walks.set(next);
                next
            });
            if walk == 2 {
                let path = LOAD_PATH.with(|path| path.replace(std::ptr::null()));
                LOADED.with(|handle| {
                    handle.set(libc::dlopen(path, libc::RTLD_NOW | libc::RTLD_LOCAL))
                });
            }
        }
        next(callback, data)
    }
}

static HOOK_CALLS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn hooked_getpid() -> libc::pid_t {
    HOOK_CALLS.fetch_add(1, Ordering::Relaxed);
    // The executable is excluded from patching, so this calls the original.
    unsafe { libc::getpid() }
}

struct Library(*mut c_void);

impl Drop for Library {
    fn drop(&mut self) {
        unsafe { libc::dlclose(self.0) };
    }
}

fn protection(addr: usize) -> i32 {
    read_proc_maps()
        .iter()
        .find(|entry| addr >= entry.start && addr < entry.end)
        .expect("address is mapped")
        .prot
}

#[test]
fn late_load_preserves_data_and_relro_permissions() {
    let dir = tempfile::tempdir().unwrap();
    for full_relro in [false, true] {
        let so = dir.path().join(format!("late-{full_relro}.so"));
        let status = Command::new("cc")
            .args(["-shared", "-fPIC", "-fplt", "-Wl,-z,relro"])
            .arg(if full_relro {
                "-Wl,-z,now"
            } else {
                "-Wl,-z,lazy"
            })
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/late_load.c"
            ))
            .arg("-o")
            .arg(&so)
            .status()
            .expect("cc should be available");
        assert!(status.success());
        let path = CString::new(so.to_str().unwrap()).unwrap();
        WALKS.with(|walks| walks.set(0));
        LOADED.with(|handle| handle.set(std::ptr::null_mut()));
        LOAD_PATH.with(|pending| pending.set(path.as_ptr()));

        let result =
            unsafe { hook_symbol_excluding_self(c"getpid", hooked_getpid as *const () as usize) }
                .unwrap();
        LOAD_PATH.with(|pending| pending.set(std::ptr::null()));
        let handle = LOADED.with(Cell::get);
        assert!(!handle.is_null(), "late dlopen failed");
        // This test relies on the double walk to land the `dlopen` after the lookup bet before the
        // patch. It's an implementation detail that could change in the future. In this case, the
        // test needs to be updated.
        assert_eq!(
            WALKS.with(Cell::get),
            2,
            "expected lookup walk + patch walk"
        );
        let library = Library(handle);
        assert!(result.entries_patched > 0);
        assert_eq!(result.entries_failed, 0);

        let mut slot = None;
        iterate_libraries(|info, _| unsafe {
            if info.dlpi_name.is_null() || CStr::from_ptr(info.dlpi_name) != path.as_c_str() {
                return false;
            }
            let info = DynamicInfo::from_phdr(info).unwrap();
            for reloc in info.relas().iter().chain(info.jmprels()) {
                if info.sym_name(libdd_gotter::elf64_r_sym(reloc.r_info)) == Some(c"getpid") {
                    slot = Some(usize::try_from(reloc.r_offset).unwrap() + info.base_address());
                }
            }
            true
        });
        let expected = if full_relro {
            PROT_READ
        } else {
            PROT_READ | PROT_WRITE
        };
        assert_eq!(protection(slot.expect("getpid relocation")), expected);

        unsafe {
            let data = libc::dlsym(handle, c"fixture_data".as_ptr());
            let call = libc::dlsym(handle, c"fixture_call".as_ptr());
            assert!(!data.is_null() && !call.is_null());
            let data: unsafe extern "C" fn() -> *mut i32 = std::mem::transmute(data);
            let call: unsafe extern "C" fn() -> libc::pid_t = std::mem::transmute(call);
            assert_eq!(protection(data() as usize), PROT_READ | PROT_WRITE);
            let before = HOOK_CALLS.load(Ordering::Relaxed);
            assert_eq!(call(), libc::getpid());
            assert!(HOOK_CALLS.load(Ordering::Relaxed) > before);
            assert_eq!(*data(), 1);
        }
        drop(library);
    }
}
