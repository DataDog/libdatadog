// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process;
use std::{fs, path::PathBuf};

use anyhow::Context;
use bin_tests::{build_artifacts, ArtifactType, ArtifactsBuild, BuildProfile};

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_debug() {
    test_crash_tracking_bin(BuildProfile::Debug, "donothing");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_sigpipe() {
    test_crash_tracking_bin(BuildProfile::Debug, "sigpipe");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_sigchld() {
    test_crash_tracking_bin(BuildProfile::Debug, "sigchld");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_sigchld_exec() {
    test_crash_tracking_bin(BuildProfile::Debug, "sigchld_exec");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_sigstack() {
    test_crash_tracking_bin(BuildProfile::Release, "donothing_sigstack");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_sigpipe_sigstack() {
    test_crash_tracking_bin(BuildProfile::Release, "sigpipe_sigstack");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_sigchld_sigstack() {
    test_crash_tracking_bin(BuildProfile::Release, "sigchld_sigstack");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_chained() {
    test_crash_tracking_bin(BuildProfile::Release, "chained");
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_fork() {
    test_crash_tracking_bin(BuildProfile::Release, "fork");
}

fn test_crash_tracking_bin(crash_tracking_receiver_profile: BuildProfile, mode: &str) {
    let (crashtracker_bin, crashtracker_receiver) =
        setup_crashtracking_crates(crash_tracking_receiver_profile);
    let fixtures = setup_test_fixtures(&[&crashtracker_receiver, &crashtracker_bin]);

    let mut p = process::Command::new(&fixtures.artifacts[&crashtracker_bin])
        .arg(format!("file://{}", fixtures.crash_profile_path.display()))
        .arg(fixtures.artifacts[&crashtracker_receiver].as_os_str())
        .arg(&fixtures.output_dir)
        .arg(mode)
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

    // Check the crash data
    let crash_profile = fs::read(fixtures.crash_profile_path)
        .context("reading crashtracker profiling payload")
        .unwrap();
    let crash_payload = serde_json::from_slice::<serde_json::Value>(&crash_profile)
        .context("deserializing crashtracker profiling payload to json")
        .unwrap();
    assert_eq!(
        serde_json::json!({
          "profiler_collecting_sample": 1,
          "profiler_inactive": 0,
          "profiler_serializing": 0,
          "profiler_unwinding": 0
        }),
        crash_payload["counters"],
    );
    assert_eq!(
        serde_json::json!({
          "signum": 11,
          "signame": "SIGSEGV",
          "faulting_address": 0,
        }),
        crash_payload["siginfo"]
    );

    let crash_telemetry = fs::read(fixtures.crash_telemetry_path)
        .context("reading crashtracker telemetry payload")
        .unwrap();
    assert_telemetry_message(&crash_telemetry);

    // Crashtracking signal handler chaining tests, as well as other tests, might only be able to
    // influence system state after the main application has crashed, and has therefore lost the
    // ability to influence the outcome of the test.  Those tests should create an "INVALID" file
    // in the output directory.
    // - If the file exists and contains only a single 'O' character, the test passes
    // - Likewise, if the file does not exist, the test passes
    // - Tests are free to output additional information in the file in case of a failure; it'll be
    //   read here
    let invalid_path = format!("{0}/INVALID", fixtures.output_dir.display());
    if let Ok(invalid) = fs::read(invalid_path) {
        assert_eq!(invalid, b"O");
    }
}

fn assert_telemetry_message(crash_telemetry: &[u8]) {
    let telemetry_payload: serde_json::Value =
        serde_json::from_slice::<serde_json::Value>(crash_telemetry)
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
            "profiler_unwinding:0",
            "profiler_collecting_sample:1",
            "profiler_inactive:0",
            "profiler_serializing:0",
            "signame:SIGSEGV",
            "faulting_address:0x0000000000000000",
        ]),
        tags
    );
    assert_eq!(telemetry_payload["payload"][0]["is_sensitive"], true);
}

#[test]
#[cfg_attr(miri, ignore)]
#[cfg(unix)]
#[allow(clippy::zombie_processes)]
fn crash_tracking_empty_endpoint() {
    use std::os::unix::net::UnixListener;

    let (crashtracker_bin, crashtracker_receiver) = setup_crashtracking_crates(BuildProfile::Debug);
    let fixtures = setup_test_fixtures(&[&crashtracker_receiver, &crashtracker_bin]);

    let socket_path = extend_path(fixtures.tmpdir.path(), "trace_agent.socket");
    let listener = UnixListener::bind(&socket_path).unwrap();

    process::Command::new(&fixtures.artifacts[&crashtracker_bin])
        // empty url, endpoint will be set to none
        .arg("")
        .arg(fixtures.artifacts[&crashtracker_receiver].as_os_str())
        .arg(&fixtures.output_dir)
        .arg("donothing")
        .env(
            "DD_TRACE_AGENT_URL",
            format!("unix://{}", socket_path.display()),
        )
        .spawn()
        .unwrap();

    let (mut stream, _) = listener.accept().unwrap();
    let mut out = vec![0; 65536];
    let read = stream.read(&mut out).unwrap();

    stream
        .write_all(b"HTTP/1.1 404\r\nContent-Length: 0\r\n\r\n")
        .unwrap();
    let resp = String::from_utf8_lossy(&out[..read]);
    let pos = resp.find("\r\n\r\n").unwrap();
    let body = &resp[pos + 4..];
    assert_telemetry_message(body.as_bytes());
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
