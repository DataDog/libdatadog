// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod modes;
pub mod test_runner;
pub mod test_types;
pub mod validation;

use std::{collections::HashMap, env, ops::DerefMut, path::PathBuf, process, sync::Mutex};

use once_cell::sync::OnceCell;

/// This crate implements an abstraction over compilation with cargo with the purpose
/// of testing full binaries or dynamic libraries, instead of just rust static libraries.
///
/// The main entrypoint is `fn build_artifacts` which takes a list of artifacts to build,
/// either executable crates, cdylib, or extra binaries, invokes cargo and return the path
/// of the built artifact.
///
/// Builds are cached between invocations so that multiple tests can use the same artifact
/// without doing expensive work twice.
///
/// It is assumed that functions in this crate are invoked in the context of a cargo #[test]
/// item, or a `cargo run` command to be able to locate artifacts built by cargo from the position
/// of the current binary.

#[derive(Debug, Default, PartialEq, Eq, Hash, Clone, Copy)]
pub enum ArtifactType {
    #[default]
    ExecutablePackage,
    CDylib,
    Bin,
}

#[derive(Debug, Default, PartialEq, Eq, Hash, Clone, Copy)]
pub enum BuildProfile {
    #[default]
    Debug,
    Release,
}

#[derive(Debug, Default, PartialEq, Eq, Hash, Clone)]
pub struct ArtifactsBuild {
    pub name: String,
    pub lib_name_override: Option<String>,
    pub artifact_type: ArtifactType,
    pub build_profile: BuildProfile,
    pub triple_target: Option<String>,
}

fn inner_build_artifact(c: &ArtifactsBuild) -> anyhow::Result<PathBuf> {
    let mut build_cmd = process::Command::new(env!("CARGO"));
    build_cmd.arg("build");
    if let BuildProfile::Release = c.build_profile {
        build_cmd.arg("--release");
    }
    match c.artifact_type {
        ArtifactType::ExecutablePackage | ArtifactType::CDylib => build_cmd.arg("-p"),
        ArtifactType::Bin => build_cmd.arg("--bin"),
    };
    build_cmd.arg(&c.name);

    // Explicitly pass RUSTFLAGS if present to ensure instrumentation
    // This is important for coverage collection when tests spawn separate binaries
    if let Ok(rustflags) = env::var("RUSTFLAGS") {
        build_cmd.env("RUSTFLAGS", rustflags);
    }

    let output = build_cmd.output().unwrap();
    if !output.status.success() {
        anyhow::bail!(
            "Cargo build failed: status code {:?}\nstderr:\n {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// This static variable contains the path in which cargo puts it's build artifacts
    /// This relies on the assumption that the current binary is assumed to not have been moved from
    /// it's directory
    static ARTIFACT_DIR: OnceCell<PathBuf> = OnceCell::new();
    let artifact_dir = ARTIFACT_DIR.get_or_init(|| {
        // If the CARGO_TARGET_DIR env var is set, then just use that.
        if let Ok(env_target_dir) = env::var("CARGO_TARGET_DIR") {
            return PathBuf::from(env_target_dir);
        }

        let test_bin_location = PathBuf::from(env::args().next().unwrap());
        let mut location_components = test_bin_location.components().rev().peekable();
        loop {
            let Some(c) = location_components.peek() else {
                break;
            };
            if c.as_os_str() == "target" {
                break;
            }
            location_components.next();
        }
        location_components.rev().collect::<PathBuf>()
    });

    let mut artifact_path = artifact_dir.clone();
    artifact_path.push(match c.build_profile {
        BuildProfile::Debug => "debug",
        BuildProfile::Release => "release",
    });

    match c.artifact_type {
        ArtifactType::ExecutablePackage | ArtifactType::Bin => artifact_path.push(&c.name),
        ArtifactType::CDylib => {
            let name = "lib".to_owned()
                + c.lib_name_override
                    .as_deref()
                    .unwrap_or(&c.name.replace('-', "_"))
                + "."
                + shared_lib_extension(
                    c.triple_target
                        .as_deref()
                        .unwrap_or(current_platform::CURRENT_PLATFORM),
                )?;
            println!("NAME: {}", name);
            artifact_path.push(name);
        }
    };
    Ok(artifact_path)
}

/// Caches and returns the path of the artifacts built by cargo
/// This function should only be called from cargo tests
pub fn build_artifacts<'b>(
    crates: &[&'b ArtifactsBuild],
) -> anyhow::Result<HashMap<&'b ArtifactsBuild, PathBuf>> {
    static ARTIFACTS: OnceCell<Mutex<HashMap<ArtifactsBuild, PathBuf>>> = OnceCell::new();

    let mut res = HashMap::new();

    let artifacts = ARTIFACTS.get_or_init(|| Mutex::new(HashMap::new()));
    for &c in crates {
        let mut artifacts = artifacts.lock().unwrap();
        let artifacts = artifacts.deref_mut();

        if artifacts.contains_key(c) {
            res.insert(c, artifacts.get(c).unwrap().clone());
        } else {
            let p = inner_build_artifact(c)?;
            res.insert(c, p.clone());
            artifacts.insert(c.clone(), p);
        }
    }

    Ok(res)
}

fn shared_lib_extension(triple_target: &str) -> anyhow::Result<&'static str> {
    let (_arch, rest) = triple_target
        .split_once('-')
        .ok_or_else(|| anyhow::anyhow!("malformed triple target {}", triple_target))?;
    Ok(
        if rest.starts_with("unknown-linux") || rest.starts_with("alpine-linux") {
            "so"
        } else if rest.starts_with("pc-windows") {
            "dll"
        } else if rest.starts_with("apple-darwin") {
            "dylib"
        } else {
            return Err(anyhow::anyhow!(
                "unrecognized triple-target {}",
                triple_target
            ));
        },
    )
}

#[macro_export]
macro_rules! timeit {
    ($op_name:literal, $op:block) => {{
        let start = std::time::Instant::now();
        let res = $op;
        let delta = start.elapsed();
        println!(
            concat!($op_name, " took {} ms"),
            delta.as_secs_f64() * 1000.0
        );
        res
    }};
}

/// Propagates `LLVM_PROFILE_FILE` to a spawned process for coverage collection.
///
/// This function is essential for integration tests that spawn separate processes to ensure
/// those child processes contribute their coverage data. It propagates **only** the
/// `LLVM_PROFILE_FILE` environment variable, which contains a pattern with `%p` (process ID)
/// that the LLVM profiling runtime expands at runtime to ensure each process writes to a
/// unique coverage file.
///
/// # How It Works
///
/// 1. **Parent propagates the pattern string:** ```
///    LLVM_PROFILE_FILE="target/llvm-cov-target/profraw/cargo-test-%p-%m.profraw" ``` Note: `%p`
///    and `%m` are NOT expanded yet - they're literal characters in the string.
///
/// 2. **Each process expands the pattern at runtime:**
///    - Parent (PID 1000): `%p` → `1000` → writes to `cargo-test-1000-abc.profraw`
///    - Child  (PID 1001): `%p` → `1001` → writes to `cargo-test-1001-def.profraw`
///    - Result: Each process writes to a unique file!
///
/// 3. **Report merges all files:**
///    - `cargo llvm-cov report` finds all `.profraw` files in the target directory
///    - Merges them into a unified coverage report
///
/// # Why Only `LLVM_PROFILE_FILE`?
///
/// Other `cargo-llvm-cov` variables like `CARGO_LLVM_COV` and `CARGO_LLVM_COV_TARGET_DIR`
/// are for `cargo` commands during build time, not for runtime binaries. Spawned test
/// binaries only need `LLVM_PROFILE_FILE` to write their coverage data.
///
/// # Arguments
/// * `cmd` - The Command to add environment variables to (before spawning)
///
/// # Example
/// ```no_run
/// use bin_tests::propagate_coverage_env;
/// use std::process::Command;
///
/// let mut cmd = Command::new("path/to/test_binary");
/// cmd.arg("--test-arg");
///
/// // Propagate coverage - spawned process will write coverage if instrumented
/// propagate_coverage_env(&mut cmd);
///
/// let child = cmd.spawn().expect("Failed to spawn");
/// ```
pub fn propagate_coverage_env(cmd: &mut process::Command) {
    // LLVM_PROFILE_FILE tells the instrumented binary where to write coverage data.
    //
    // This variable contains patterns like %p (process ID) and %m (module signature).
    // The LLVM profiling runtime inside each instrumented binary expands these patterns
    // at runtime, ensuring each spawned process writes to a unique file:
    //
    // Pattern:  "cargo-test-%p-%m.profraw"
    // PID 1000: "cargo-test-1000-abc123.profraw"
    // PID 1001: "cargo-test-1001-def456.profraw"
    //
    // This is the ONLY variable needed for spawned binaries to contribute coverage.
    // CARGO_LLVM_COV* variables are for cargo commands, not runtime binaries.
    if let Ok(profile_file) = env::var("LLVM_PROFILE_FILE") {
        cmd.env("LLVM_PROFILE_FILE", profile_file);
    }
}
