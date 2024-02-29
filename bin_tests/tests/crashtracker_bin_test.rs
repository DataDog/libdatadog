// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

#![cfg(unix)]

use std::path::Path;
use std::process;
use std::{fs, path::PathBuf};

use anyhow::Context;
use bin_tests::{build_artifacts, ArtifactType, ArtifactsBuild, BuildProfile};

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_debug() {
    test_crash_tracking_bin(BuildProfile::Debug);
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_release() {
    test_crash_tracking_bin(BuildProfile::Release);
}

fn test_crash_tracking_bin(crash_tracking_receiver_profile: BuildProfile) {
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
    let artifacts = build_artifacts(&[&crashtracker_receiver, &crashtracker_bin]).unwrap();

    let tmpdir = tempfile::TempDir::new().unwrap();

    let crash_profile_path = extend_path(tmpdir.path(), "crash");
    let crash_telemetry_path = extend_path(tmpdir.path(), "crash.telemetry");
    let stdout_path = extend_path(tmpdir.path(), "out.stdout");
    let stderr_path = extend_path(tmpdir.path(), "out.stderr");

    let mut p = process::Command::new(&artifacts[&crashtracker_bin])
        .arg(&crash_profile_path)
        .arg(artifacts[&crashtracker_receiver].as_os_str())
        .arg(&stderr_path)
        .arg(&stdout_path)
        .spawn()
        .unwrap();
    let exit_status = bin_tests::timeit!("exit after signal", {
        eprintln!("Waiting for exit");
        p.wait().unwrap()
    });
    assert!(!exit_status.success());
    // Sadly this is necessary because in case of partial crash the tracked process
    // doesn't wait for the crahtracker receiver which causes
    std::thread::sleep(std::time::Duration::from_millis(100));

    let stderr = fs::read(stderr_path)
        .context("reading crashtracker stderr")
        .unwrap();
    let stdout = fs::read(stdout_path)
        .context("reading crashtracker stdout")
        .unwrap();
    assert!(matches!(
        String::from_utf8(stderr).as_deref(),
        Ok("") | Ok("Failed to fully receive crash.  Exit state was: StackTrace([])\n")
    ));
    assert_eq!(Ok(""), String::from_utf8(stdout).as_deref());

    // Check the crash data
    let crash_profile = fs::read(crash_profile_path)
        .context("reading crashtracker profiling payload")
        .unwrap();
    let crash_payload = serde_json::from_slice::<serde_json::Value>(&crash_profile)
        .context("deserializing crashtracker profiling payload to json")
        .unwrap();
    assert_eq!(
        serde_json::json!({
          "collecting_sample": 1,
          "not_profiling": 0,
          "unwinding": 0,
          "serializing": 0
        }),
        crash_payload["counters"],
    );
    assert_eq!(
        serde_json::json!({
          "signum": 11,
          "signame": "SIGSEGV"
        }),
        crash_payload["siginfo"]
    );

    let crash_telemetry = fs::read(crash_telemetry_path)
        .context("reading crashtracker telemetry payload")
        .unwrap();
    let telemetry_payload = serde_json::from_slice::<serde_json::Value>(&crash_telemetry)
        .context("deserializing crashtracker telemetry payload to json")
        .unwrap();
    assert_eq!(telemetry_payload["request_type"], "logs");
    assert_eq!(
        serde_json::json!({
          "service_name": "foo",
          "service_version": "bar",
          "language_name": "native",
          "language_version": "unknown",
          "tracer_version": "unknown"
        }),
        telemetry_payload["application"]
    );
    assert_eq!(telemetry_payload["payload"].as_array().unwrap().len(), 1);

    let tags = telemetry_payload["payload"][0]["tags"]
        .as_str()
        .unwrap()
        .split(',')
        .filter(|t| !t.starts_with("uuid:"))
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        std::collections::HashSet::from_iter([
            "signum:11",
            "signame:SIGSEGV",
            "collecting_sample:1",
            "not_profiling:0",
            "serializing:0",
            "unwinding:0",
        ]),
        tags
    );
    assert_eq!(telemetry_payload["payload"][0]["is_sensitive"], true);
}

fn extend_path<T: AsRef<Path>>(parent: &Path, path: T) -> PathBuf {
    let mut parent = parent.to_path_buf();
    parent.push(path);
    parent
}
