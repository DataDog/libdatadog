// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Demonstrates using `libdd-heap-gotter-ffi` as an actual dynamically-loaded C ABI library.
//!
//! Build the cdylib first:
//! ```sh
//! cargo build -p libdd-heap-gotter-ffi
//! ```
//!
//! Then run this demo:
//! ```sh
//! cargo run -p libdd-heap-gotter-ffi --example cdylib_demo
//! ```
//!
//! The gotter-ffi crate is Linux-only; on other targets the example
//! compiles to a no-op `main` so clippy/test on non-Linux don't fail
//! with "configured out".

#[cfg(not(target_os = "linux"))]
fn main() -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), String> {
    linux::main()
}

#[cfg(target_os = "linux")]
mod linux {
    use libdd_common_ffi::VoidResult;
    use std::ffi::{CStr, CString};
    use std::path::{Path, PathBuf};
    use std::thread::sleep;
    use std::time::Duration;

    const LIB_NAME: &str = "liblibdd_heap_gotter_ffi.so";

    type InstallFn = unsafe extern "C" fn() -> VoidResult;
    type IsInstalledFn = unsafe extern "C" fn() -> bool;

    struct DlopenHandle(*mut libc::c_void);

    impl DlopenHandle {
        fn open(path: &Path) -> Result<Self, String> {
            let path =
                CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if handle.is_null() {
                return Err(dlerror());
            }
            Ok(Self(handle))
        }

        unsafe fn symbol<T>(&self, name: &CStr) -> Result<T, String>
        where
            T: Copy,
        {
            let ptr = unsafe { libc::dlsym(self.0, name.as_ptr()) };
            if ptr.is_null() {
                return Err(dlerror());
            }
            Ok(unsafe { std::mem::transmute_copy(&ptr) })
        }
    }

    impl Drop for DlopenHandle {
        fn drop(&mut self) {
            unsafe {
                libc::dlclose(self.0);
            }
        }
    }

    fn dlerror() -> String {
        let err = unsafe { libc::dlerror() };
        if err.is_null() {
            "unknown dlerror".to_string()
        } else {
            unsafe { CStr::from_ptr(err) }
                .to_string_lossy()
                .into_owned()
        }
    }

    fn cdylib_path() -> Result<PathBuf, String> {
        if let Some(path) = std::env::var_os("DDOG_HEAP_GOTTER_FFI_CDYLIB") {
            return Ok(PathBuf::from(path));
        }

        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let profile_dir = exe
            .parent()
            .and_then(|p| p.parent())
            .ok_or_else(|| format!("could not infer target profile dir from {}", exe.display()))?;
        Ok(profile_dir.join(LIB_NAME))
    }

    fn check(result: VoidResult, operation: &str) -> Result<(), String> {
        match result {
            VoidResult::Ok => Ok(()),
            VoidResult::Err(err) => Err(format!("{operation} failed: {err}")),
        }
    }

    pub fn main() -> Result<(), String> {
        let lib_path = cdylib_path()?;
        if !lib_path.exists() {
            return Err(format!(
                "{} does not exist; run `cargo build -p libdd-heap-gotter-ffi` first",
                lib_path.display()
            ));
        }

        println!("pid={}", std::process::id());
        println!("loading {}", lib_path.display());
        let lib = DlopenHandle::open(&lib_path)?;

        let install: InstallFn = unsafe { lib.symbol(c"ddog_heap_gotter_install")? };
        let is_installed: IsInstalledFn = unsafe { lib.symbol(c"ddog_heap_gotter_is_installed")? };

        println!("pre-install is_installed={}", unsafe { is_installed() });
        check(unsafe { install() }, "ddog_heap_gotter_install")?;
        println!("post-install is_installed={}", unsafe { is_installed() });
        println!("attach a tracer on `usdt:*:ddheap:*`; producing allocation pressure...");

        for i in 0..30_u64 {
            let parts: Vec<String> = (0..1000)
                .map(|j| format!("chunk-{i}-{j}-with-some-padding-to-make-it-meaningful"))
                .collect();
            let joined = parts.join(", ");
            println!("[{i}] joined {} bytes", joined.len());
            sleep(Duration::from_secs(1));
        }

        // Installation is permanent - there is no un-install. The GOT entries
        // patched by install point at functions in this cdylib, so it must stay
        // loaded for the life of the process; unloading it would leave dangling
        // function pointers. Leak the handle to make that explicit.
        std::mem::forget(lib);
        Ok(())
    }
} // mod linux
