// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::arch;
use crate::module::Module;
use crate::utils::project_root;
use anyhow::Result;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use tools::headers::dedup_headers;

pub struct CrashTracker {
    pub arch: Rc<str>,
    pub base_header: Rc<str>,
    pub profile: Rc<str>,
    pub source_include: Rc<str>,
    pub target_dir: Rc<str>,
    pub target_include: Rc<str>,
}

impl CrashTracker {
    fn gen_binaries(&self) -> Result<()> {
        if arch::BUILD_CRASHTRACKER {
            let mut datadog_root = project_root();
            datadog_root.push(self.target_dir.as_ref());

            let mut crashtracker_dir = project_root();
            crashtracker_dir.push("libdd-crashtracker");
            let mut config = cmake::Config::new(crashtracker_dir.to_str().unwrap());
            let config = config
                .define("Datadog_ROOT", datadog_root.to_str().unwrap())
                .define("CMAKE_INSTALL_PREFIX", self.target_dir.to_string());
            let config = if self.arch.as_ref() == "x86_64-apple-darwin" {
                // Set environment variables for target OS and arch
                std::env::set_var("CARGO_CFG_TARGET_OS", "macos");
                std::env::set_var("CARGO_CFG_TARGET_ARCH", "x86_64");

                config.define("CMAKE_OSX_ARCHITECTURES", "x86_64")
            } else {
                config
            };

            let dst = config.build();

            // Copy the built binary to the target bin directory
            let binary_name = "libdatadog-crashtracking-receiver";
            let target_binary = PathBuf::from(self.target_dir.as_ref())
                .join("bin")
                .join(binary_name);

            // The CMake install location depends on whether target_dir is absolute or relative
            let cmake_installed_binary = if PathBuf::from(self.target_dir.as_ref()).is_absolute() {
                // For absolute paths, CMake installs directly to target_dir/bin
                PathBuf::from(self.target_dir.as_ref())
                    .join("bin")
                    .join(binary_name)
            } else {
                // For relative paths, CMake installs to build/target_dir/bin
                dst.join("build")
                    .join(self.target_dir.as_ref())
                    .join("bin")
                    .join(binary_name)
            };

            // Check if source and target are the same path
            if cmake_installed_binary == target_binary {
                let metadata = fs::metadata(&cmake_installed_binary)?;
                anyhow::ensure!(
                    metadata.len() > 0,
                    "CMake built {} but it's empty",
                    binary_name
                );
                return Ok(());
            }

            if cmake_installed_binary.exists() {
                let metadata = fs::metadata(&cmake_installed_binary)?;
                anyhow::ensure!(
                    metadata.len() > 0,
                    "CMake built {} but it's empty",
                    binary_name
                );

                fs::copy(&cmake_installed_binary, &target_binary)?;

                let target_metadata = fs::metadata(&target_binary)?;
                anyhow::ensure!(target_metadata.len() > 0, "Copied {} is empty", binary_name);
            } else {
                anyhow::bail!(
                    "CMake did not produce {} at {}",
                    binary_name,
                    cmake_installed_binary.display()
                );
            }
        }

        Ok(())
    }
    fn add_headers(&self) -> Result<()> {
        let origin_path: PathBuf = [self.source_include.as_ref(), "crashtracker.h"]
            .iter()
            .collect();
        let target_path: PathBuf = [self.target_include.as_ref(), "crashtracker.h"]
            .iter()
            .collect();

        let headers = vec![target_path.to_str().unwrap()];
        fs::copy(origin_path, &target_path).expect("Failed to copy crashtracker.h");

        dedup_headers(self.base_header.as_ref(), &headers);

        Ok(())
    }
}

impl Module for CrashTracker {
    fn build(&self) -> Result<()> {
        Ok(())
    }

    fn install(&self) -> Result<()> {
        self.add_headers()?;
        self.gen_binaries()?;
        Ok(())
    }
}
