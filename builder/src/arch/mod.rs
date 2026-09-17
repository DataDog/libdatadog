// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::utils::{file_replace, project_root};

pub fn generate_pkg_config(
    crate_path: &str,
    target_path: &str,
    version: &str,
    native_libs: &str,
) -> Result<()> {
    let files: [&str; 3] = [
        "datadog_profiling.pc",
        "datadog_profiling_with_rpath.pc",
        "datadog_profiling-static.pc",
    ];

    let pc_dir = Path::new(target_path);
    fs::create_dir_all(pc_dir).expect("Failed to create pkgconfig directory");

    for file in files.iter() {
        let file_in = file.to_string() + ".in";

        let mut pc_origin: PathBuf = project_root();
        pc_origin.push(crate_path);
        pc_origin.push(file_in);

        let pc_target: PathBuf = [pc_dir.as_os_str(), OsStr::new(file)].iter().collect();

        file_replace(
            pc_origin.to_str().unwrap(),
            pc_target.to_str().unwrap(),
            "@Datadog_VERSION@",
            version,
        )?;

        if *file == files[2] {
            file_replace(
                pc_origin.to_str().unwrap(),
                pc_target.to_str().unwrap(),
                "@Datadog_LIBRARIES@",
                native_libs,
            )?;
        }
    }
    Ok(())
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod linux;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub use crate::arch::linux::*;

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[cfg(all(target_os = "linux", target_env = "musl"))]
mod musl;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[cfg(all(target_os = "linux", target_env = "musl"))]
pub use crate::arch::musl::*;

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[cfg(target_os = "macos")]
pub mod apple;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[cfg(target_os = "macos")]
pub use crate::arch::apple::*;

#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
#[cfg(target_os = "windows")]
pub use crate::arch::windows::*;
