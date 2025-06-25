// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libc::execve;
use nix::errno::Errno;
use std::ffi::CString;

// The args_cstrings and env_vars_strings fields are just storage.  Even though they're
// unreferenced, they're a necessary part of the struct.
#[allow(dead_code)]
pub struct PreparedExecve {
    binary_path: CString,
    args_cstrings: Vec<CString>,
    args_ptrs: Vec<*const libc::c_char>,
    env_vars_cstrings: Vec<CString>,
    env_vars_ptrs: Vec<*const libc::c_char>,
}

impl PreparedExecve {
    pub fn new(binary_path: &str, args: &[String], env: &[(String, String)]) -> Self {
        // Allocate and store binary path
        #[allow(clippy::expect_used)]
        let binary_path =
            CString::new(binary_path).expect("Failed to convert binary path to CString");

        // Allocate and store arguments
        #[allow(clippy::expect_used)]
        let args_cstrings: Vec<CString> = args
            .iter()
            .map(|s| CString::new(s.as_str()).expect("Failed to convert argument to CString"))
            .collect();
        let args_ptrs: Vec<*const libc::c_char> = args_cstrings
            .iter()
            .map(|arg| arg.as_ptr())
            .chain(std::iter::once(std::ptr::null())) // Adds a null pointer to the end of the list
            .collect();

        // Allocate and store environment variables
        let env_vars_cstrings: Vec<CString> = env
            .iter()
            .map(|(key, value)| {
                let env_str = format!("{key}={value}");
                #[allow(clippy::expect_used)]
                CString::new(env_str).expect("Failed to convert environment variable to CString")
            })
            .collect();
        let env_vars_ptrs: Vec<*const libc::c_char> = env_vars_cstrings
            .iter()
            .map(|env| env.as_ptr())
            .chain(std::iter::once(std::ptr::null())) // Adds a null pointer to the end of the list
            .collect();

        Self {
            binary_path,
            args_cstrings,
            args_ptrs,
            env_vars_cstrings,
            env_vars_ptrs,
        }
    }

    /// Calls `execve` on the prepared arguments.
    pub fn exec(&self) -> Result<(), Errno> {
        // Safety: the only way to make one of these is through `new`, which ensures that everything
        // is well-formed.
        unsafe {
            if execve(
                self.binary_path.as_ptr(),
                self.args_ptrs.as_ptr(),
                self.env_vars_ptrs.as_ptr(),
            ) == -1
            {
                Err(Errno::last())
            } else {
                Ok(())
            }
        }
    }
}
