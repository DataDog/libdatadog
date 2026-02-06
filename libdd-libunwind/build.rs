// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

fn main() {
    #[cfg(target_os = "linux")]
    linux::main();
}

#[cfg(target_os = "linux")]
mod linux {
    use std::env;
    use std::path::PathBuf;

    pub(crate) fn main() {
        let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
        let build_dir = out_dir.join("libunwind_build");
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let libunwind_dir = std::path::Path::new(&manifest_dir).join("libunwind");

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

            eprintln!("Configuring and building libunwind...");
            let status = std::process::Command::new("sh")
                .current_dir(&build_dir)
                .args([
                    "-c",
                    &format!(
                        "{}/configure --disable-shared --enable-static --disable-minidebuginfo --disable-zlibdebuginfo --disable-tests && make -j$(nproc)",
                        libunwind_dir.display()
                    )
                ])
                .status()
                .expect("Failed to run configure/make");

            if !status.success() {
                panic!("libunwind build failed with exit code: {:?}", status.code());
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

        // Link directives for this crate
        println!("cargo:rustc-link-search=native={}", lib_path.display());
        println!("cargo:rustc-link-lib=static=unwind");

        // Export paths to dependent crates via DEP_UNWIND_* environment variables
        // These are automatically passed to crates that depend on us
        println!("cargo:include={}", include_path.display());
        println!("cargo:lib={}", lib_path.display());
        println!("cargo:libdir={}", lib_path.display()); // Alternative name
        println!("cargo:root={}", build_dir.display());

        eprintln!("libunwind library ready at {}", lib_path.display());

        // More specific rerun triggers
        println!("cargo:rerun-if-changed={}/src", libunwind_dir.display());
        println!("cargo:rerun-if-changed={}/include", libunwind_dir.display());
        println!("cargo:rerun-if-changed=build.rs");
    }
}
