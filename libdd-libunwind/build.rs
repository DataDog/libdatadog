// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

fn main() {
    #[cfg(target_os = "linux")]
    linux::main();
}

#[cfg(target_os = "linux")]
mod linux {
    use std::env;
    use std::path::{Path, PathBuf};

    const LIBUNWIND_REPO: &str = "https://github.com/DataDog/libunwind";
    const LIBUNWIND_BRANCH: &str = "kevin/v1.8.1-custom-2";

    fn clone_libunwind(out_dir: &Path) -> PathBuf {
        let source_dir = out_dir.join("libunwind-src");
        if source_dir.exists() {
            eprintln!("Using cached libunwind source");
            return source_dir;
        }

        eprintln!("Cloning libunwind from {LIBUNWIND_REPO} (branch: {LIBUNWIND_BRANCH})...");
        let status = std::process::Command::new("git")
            .env("HOME", out_dir)
            .args([
                "clone",
                "--depth=1",
                "--branch",
                LIBUNWIND_BRANCH,
                LIBUNWIND_REPO,
                source_dir.to_str().unwrap(),
            ])
            .status()
            .expect("Failed to run git. Is git installed?");
        assert!(status.success(), "Failed to clone libunwind");

        source_dir
    }

    pub(crate) fn main() {
        let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
        let build_dir = out_dir.join("libunwind_build");
        let libunwind_dir = clone_libunwind(&out_dir);

        // Check if libunwind submodule is initialized
        if !libunwind_dir.exists() || std::fs::read_dir(&libunwind_dir).unwrap().next().is_none() {
            panic!(
                "libunwind submodule not initialized!\n\
                 Run: git submodule update --init --recursive\n\
                 \n\
                 For CI, ensure your workflow checks out submodules:\n\
                 - GitHub Actions: add 'submodules: recursive' to actions/checkout\n\
                 - GitLab CI: add 'GIT_SUBMODULE_STRATEGY: recursive'\n\
                 \n\
                 Directory checked: {}",
                libunwind_dir.display()
            );
        }

        std::fs::create_dir_all(&build_dir).unwrap();

        let lib_file = build_dir.join("src/.libs/libunwind.a");

        // Only build if library doesn't exist
        if !lib_file.exists() {
            eprintln!("Building libunwind from source...");

            // Only run autoreconf if configure doesn't exist
            let configure_script = libunwind_dir.join("configure");
            if !configure_script.exists() {
                eprintln!("Running autoreconf...");
                let status = std::process::Command::new("sh")
                    .current_dir(&libunwind_dir)
                    .args(["-c", "autoreconf -i"])
                    .status()
                    .expect(
                        "Failed to run autoreconf. Install with: apt install autoconf automake libtool",
                    );

                if !status.success() {
                    panic!("autoreconf failed with exit code: {:?}", status.code());
                }
            }

            eprintln!("Configuring libunwind...");
            let status = std::process::Command::new("sh")
                .current_dir(&build_dir)
                .args([
                    "-c",
                    &format!(
                        r"{}/configure CXXFLAGS=-fPIC\ -D_GLIBCXX_USE_CXX11_ABI=0\ -O3\ -g CFLAGS=-fPIC\ -O3\ -g --disable-shared --enable-static --disable-minidebuginfo --disable-zlibdebuginfo --disable-tests",
                        libunwind_dir.display()
                    )
                ])
                .status()
                .expect("Failed to run configure");

            if !status.success() {
                panic!(
                    "libunwind configure failed with exit code: {:?}",
                    status.code()
                );
            }

            eprintln!("Building libunwind...");
            let status = std::process::Command::new("sh")
                .current_dir(&build_dir)
                .args(["-c", "make -j$(nproc)"])
                .status()
                .expect("Failed to run make");

            if !status.success() {
                panic!("libunwind make failed with exit code: {:?}", status.code());
            }

            // Verify the library was actually created
            if !lib_file.exists() {
                panic!(
                    "libunwind.a was not created at expected location: {}",
                    lib_file.display()
                );
            }

            eprintln!("libunwind built successfully at {}", lib_file.display());
        } else {
            eprintln!("Using cached libunwind build");
        }

        let lib_path = build_dir.join("src/.libs");
        let include_path = build_dir.join("include");

        #[cfg(target_arch = "x86_64")]
        let arch = "x86_64";
        #[cfg(target_arch = "aarch64")]
        let arch = "aarch64";

        // Link directives for this crate
        println!("cargo:rustc-link-search=native={}", lib_path.display());
        println!("cargo:rustc-link-lib=static=unwind");
        println!("cargo:rustc-link-lib=static=unwind-{}", arch);

        // Export paths to dependent crates via DEP_UNWIND_* environment variables
        println!("cargo:include={}", include_path.display());
        println!("cargo:lib={}", lib_path.display());
        println!("cargo:libdir={}", lib_path.display());
        println!("cargo:root={}", build_dir.display());

        eprintln!("libunwind library ready at {}", lib_path.display());

        println!("cargo:rerun-if-changed=build.rs");
    }
}
