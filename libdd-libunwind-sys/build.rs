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
    use std::process::Command;

    const LIBUNWIND_REPO: &str = "https://github.com/DataDog/libunwind.git";
    const LIBUNWIND_BRANCH: &str = "kevin/v1.8.1-custom-2";

    /// Try initializing the libunwind git submodule from the repo root.
    /// Only works when running inside a git repository.
    fn try_submodule_init(repo_root: &Path) -> bool {
        if !repo_root.join(".git").exists() {
            return false;
        }
        eprintln!("Initializing libunwind submodule...");
        Command::new("git")
            .args([
                "submodule",
                "update",
                "--init",
                "--recursive",
                "--",
                "libdd-libunwind-sys/libunwind",
            ])
            .current_dir(repo_root)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Clone the libunwind repository directly into `target_dir`.
    /// Used as a fallback when not inside a git repo (e.g. cargo-semver-checks
    /// extracts baseline source into a plain directory without .git).
    fn clone_libunwind(target_dir: &Path) -> bool {
        eprintln!("Cloning libunwind source (no git repo or submodule unavailable)...");
        let _ = std::fs::remove_dir_all(target_dir);
        Command::new("git")
            .args([
                "clone",
                "--depth",
                "1",
                "--branch",
                LIBUNWIND_BRANCH,
                LIBUNWIND_REPO,
                &target_dir.to_string_lossy(),
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn ensure_libunwind_source(manifest_dir: &str, libunwind_dir: &Path) {
        if libunwind_dir.join("src").exists() {
            return;
        }

        let repo_root = Path::new(manifest_dir).parent().unwrap();

        if try_submodule_init(repo_root) && libunwind_dir.join("src").exists() {
            return;
        }

        // This can happen when running cargo-semver-checks
        // because it extracts the baseline source into a plain directory without .git
        if clone_libunwind(libunwind_dir) && libunwind_dir.join("src").exists() {
            return;
        }

        panic!(
            "Failed to obtain libunwind source at {}.\n\
             Try manually:\n  \
             git submodule update --init --recursive\n  \
             or: git clone --branch {} {} {}",
            libunwind_dir.display(),
            LIBUNWIND_BRANCH,
            LIBUNWIND_REPO,
            libunwind_dir.display()
        );
    }

    pub(crate) fn main() {
        let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
        let build_dir = out_dir.join("libunwind_build");
        let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
        let libunwind_dir = Path::new(&manifest_dir).join("libunwind");

        ensure_libunwind_source(&manifest_dir, &libunwind_dir);

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
                        r"{}/configure --enable-debug CXXFLAGS=-DUNW_DEBUG=1\ -fPIC\ -D_GLIBCXX_USE_CXX11_ABI=0\ -O3\ -g CFLAGS=-fPIC\ -O3\ -g --disable-shared --enable-static --disable-minidebuginfo --disable-zlibdebuginfo --disable-tests",
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

        println!("cargo:rerun-if-changed={}/src", libunwind_dir.display());
        println!("cargo:rerun-if-changed={}/include", libunwind_dir.display());
        println!("cargo:rerun-if-changed=build.rs");
    }
}
