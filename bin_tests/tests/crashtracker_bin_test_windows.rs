// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(windows)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process;
use std::{fs, path::PathBuf};

use anyhow::Context;
use bin_tests::{build_artifacts, ArtifactType, ArtifactsBuild, BuildProfile};
use serde_json::Value;

// This test is disabled for now on x86_64 musl and macos
// It seems that on aarch64 musl, libc has CFI which allows
// unwinding passed the signal frame.
#[test]
#[cfg_attr(miri, ignore)]
fn test_crasht_tracking_validate_callstack() {
    test_crash_tracking_callstack()
}

fn test_crash_tracking_callstack() {
    let (_, crashtracker_receiver) = setup_crashtracking_crates(BuildProfile::Release);

    let crashing_app = ArtifactsBuild {
        name: "crashing_test_app".to_owned(),
        // compile in debug so we avoid inlining
        // and can check the callchain
        build_profile: BuildProfile::Debug,
        artifact_type: ArtifactType::Bin,
        triple_target: None,
    };

    let fixtures = setup_test_fixtures(&[&crashtracker_receiver, &crashing_app]);

    let mut p = process::Command::new(&fixtures.artifacts[&crashing_app])
        .arg(format!("file://{}", fixtures.crash_profile_path.display()))
        .arg(fixtures.artifacts[&crashtracker_receiver].as_os_str())
        .arg(&fixtures.output_dir)
        .spawn()
        .unwrap();

    let exit_status = bin_tests::timeit!("exit after signal", {
        eprintln!("Waiting for exit");
        p.wait().unwrap()
    });
    assert!(!exit_status.success());

    let stderr_path = format!("{0}/out.stderr", fixtures.output_dir.display());
    let stderr = fs::read(stderr_path)
        .context("reading crashtracker stderr")
        .unwrap();
    let stdout_path = format!("{0}/out.stdout", fixtures.output_dir.display());
    let stdout = fs::read(stdout_path)
        .context("reading crashtracker stdout")
        .unwrap();
    let s = String::from_utf8(stderr);
    assert!(
        matches!(
            s.as_deref(),
            Ok("") | Ok("Failed to fully receive crash.  Exit state was: StackTrace([])\n")
            | Ok("Failed to fully receive crash.  Exit state was: InternalError(\"{\\\"ip\\\": \\\"\")\n"),
        ),
        "got {s:?}"
    );
    assert_eq!(Ok(""), String::from_utf8(stdout).as_deref());

    let crash_profile = fs::read(fixtures.crash_profile_path)
        .context("reading crashtracker profiling payload")
        .unwrap();
    let crash_payload = serde_json::from_slice::<serde_json::Value>(&crash_profile)
        .context("deserializing crashtracker profiling payload to json")
        .unwrap();

    // Note: in Release, we do not have the crate and module name prepended to the function name
    // Here we compile the crashing app in Debug.
    let mut expected_functions = Vec::new();
    // It seems that on arm/arm64, fn3 is inlined in fn2, so not present.
    // Add fn3 only for x86_64 arch
    #[cfg(target_arch = "x86_64")]
    expected_functions.push("crashing_test_app::unix::fn3");
    expected_functions.extend_from_slice(&[
        "crashing_test_app::unix::fn2",
        "crashing_test_app::unix::fn1",
        "crashing_test_app::unix::main",
        "crashing_test_app::main",
    ]);

    let crashing_callstack = &crash_payload["error"]["stack"]["frames"];
    assert!(
        crashing_callstack.as_array().unwrap().len() >= expected_functions.len(),
        "crashing thread callstacks does have less frames than expected. Current: {}, Expected: {}",
        crashing_callstack.as_array().unwrap().len(),
        expected_functions.len()
    );

    let function_names: Vec<&str> = crashing_callstack
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["function"].as_str().unwrap_or(""))
        .collect();

    for (expected, actual) in expected_functions.iter().zip(function_names.iter()) {
        assert_eq!(expected, actual);
    }
}

struct TestFixtures<'a> {
    tmpdir: tempfile::TempDir,
    crash_profile_path: PathBuf,
    crash_telemetry_path: PathBuf,
    output_dir: PathBuf,

    artifacts: HashMap<&'a ArtifactsBuild, PathBuf>,
}

fn setup_test_fixtures<'a>(crates: &[&'a ArtifactsBuild]) -> TestFixtures<'a> {
    let artifacts = build_artifacts(crates).unwrap();

    let tmpdir = tempfile::TempDir::new().unwrap();
    let dirpath = tmpdir.path();
    TestFixtures {
        crash_profile_path: extend_path(dirpath, "crash"),
        crash_telemetry_path: extend_path(dirpath, "crash.telemetry"),
        output_dir: dirpath.to_path_buf(),

        artifacts,
        tmpdir,
    }
}

fn setup_crashtracking_crates(
    crash_tracking_receiver_profile: BuildProfile,
) -> (ArtifactsBuild, ArtifactsBuild) {
    let crashtracker_bin = ArtifactsBuild {
        name: "crashtracker_bin_test".to_owned(),
        build_profile: crash_tracking_receiver_profile,
        artifact_type: ArtifactType::Bin,
        triple_target: None,
    };
    let crashtracker_receiver = ArtifactsBuild {
        name: "crashtracker_receiver".to_owned(),
        build_profile: crash_tracking_receiver_profile,
        artifact_type: ArtifactType::Bin,
        triple_target: None,
    };
    (crashtracker_bin, crashtracker_receiver)
}

fn extend_path<T: AsRef<Path>>(parent: &Path, path: T) -> PathBuf {
    let mut parent = parent.to_path_buf();
    parent.push(path);
    parent
}
