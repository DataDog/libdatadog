// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libc::c_void;
use std::ffi::CString;
use std::path::Path;
use std::path::PathBuf;

pub fn get_data_folder_path() -> std::io::Result<PathBuf> {
    Path::new(&env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .canonicalize()
}

pub struct SharedLibrary {
    handle: *mut c_void,
}

impl SharedLibrary {
    pub fn open(lib_path: &str) -> Result<Self, String> {
        let cstr = CString::new(lib_path).map_err(|e| e.to_string())?;
        // Use RTLD_NOW or another flag
        let handle = unsafe { libc::dlopen(cstr.as_ptr(), libc::RTLD_NOW) };
        if handle.is_null() {
            Err("Failed to open library".to_string())
        } else {
            Ok(Self { handle })
        }
    }

    pub fn get_symbol_address(&self, symbol: &str) -> Result<String, String> {
        let cstr = CString::new(symbol).map_err(|e| e.to_string())?;
        let sym = unsafe { libc::dlsym(self.handle, cstr.as_ptr()) };
        if sym.is_null() {
            Err(format!("Failed to find symbol: {symbol}"))
        } else {
            Ok(format!("{sym:p}"))
        }
    }
}

impl Drop for SharedLibrary {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { libc::dlclose(self.handle) };
        }
    }
}
