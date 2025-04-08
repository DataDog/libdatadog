// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

use anyhow::Context;
use libc::execve;
use nix::errno::Errno;
use std::os::fd::IntoRawFd;
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    os::fd::RawFd,
};

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

/// Opens a file for writing (in append mode) or opens /dev/null
/// * If the filename is provided, it will try to open (creating if needed) the specified file.
///   Failure to do so is an error.
/// * If the filename is not provided, it will open /dev/null Some systems can fail to provide
///   `/dev/null` (e.g., chroot jails), so this failure is also an error.
/// * Using Stdio::null() is more direct, but it will cause a panic in environments where /dev/null
///   is not available.
pub fn open_file_or_quiet(filename: Option<&str>) -> anyhow::Result<RawFd> {
    let file = filename.map_or_else(
        || File::open("/dev/null").context("Failed to open /dev/null"),
        |f| {
            OpenOptions::new()
                .append(true)
                .create(true)
                .open(f)
                .with_context(|| format!("Failed to open or create file: {f}"))
        },
    )?;
    Ok(file.into_raw_fd())
}
