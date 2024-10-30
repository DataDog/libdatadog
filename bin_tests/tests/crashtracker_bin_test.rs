// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process;
use std::{fs, path::PathBuf};

use anyhow::Context;
use bin_tests::{build_artifacts, ArtifactType, ArtifactsBuild, BuildProfile, ReceiverType};

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_debug_stdin() {
    test_crash_tracking_bin(BuildProfile::Debug, ReceiverType::ChildProcessStdin);
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_crash_tracking_bin_debug_unix_socket() {
    test_crash_tracking_bin(BuildProfile::Debug, ReceiverType::UnixSocket);
}

#[test]
#[ignore] // This test is slow, only run it if explicitly opted in
fn test_crash_tracking_bin_release_stdin() {
    test_crash_tracking_bin(BuildProfile::Release, ReceiverType::ChildProcessStdin);
}

#[test]
#[ignore] // This test is slow, only run it if explicitly opted in
fn test_crash_tracking_bin_release_unix_socket() {
    test_crash_tracking_bin(BuildProfile::Release, ReceiverType::UnixSocket);
}

fn test_crash_tracking_bin(
    crash_tracking_receiver_profile: BuildProfile,
    receiver_type: ReceiverType,
) {
    let (crashtracker_bin, crashtracker_receiver, crashtracker_unix_socket_receiver) =
        setup_crashtracking_crates(crash_tracking_receiver_profile);
    let fixtures = setup_test_fixtures(&[
        &crashtracker_receiver,
        &crashtracker_bin,
        &crashtracker_unix_socket_receiver,
    ]);

    let mut p = process::Command::new(&fixtures.artifacts[&crashtracker_bin])
        .arg(receiver_type.to_string())
        .arg(format!("file://{}", fixtures.crash_profile_path.display()))
        .arg(fixtures.artifacts[&crashtracker_receiver].as_os_str())
        .arg(fixtures.artifacts[&crashtracker_unix_socket_receiver].as_os_str())
        .arg(&fixtures.stderr_path)
        .arg(&fixtures.stdout_path)
        .arg(&fixtures.unix_socket_path)
        .spawn()
        .unwrap();
    let exit_status = bin_tests::timeit!("exit after signal", {
        eprintln!("Waiting for exit");
        p.wait().unwrap()
    });
    assert!(!exit_status.success());
    // Sadly this is necessary because in case of partial crash the tracked process
    // doesn't wait for the crahtracker receiver which causes races, with the test
    // running before the receiver has a chance to send the report.
    std::thread::sleep(std::time::Duration::from_millis(100));

    let stderr = fs::read(fixtures.stderr_path)
        .context("reading crashtracker stderr")
        .unwrap();
    let stdout = fs::read(fixtures.stdout_path)
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
fn crash_tracking_empty_endpoint_unix_socket() {
    crash_tracking_empty_endpoint_inner(ReceiverType::UnixSocket)
}

#[test]
#[cfg_attr(miri, ignore)]
#[cfg(unix)]
fn crash_tracking_empty_endpoint_stdin() {
    crash_tracking_empty_endpoint_inner(ReceiverType::ChildProcessStdin)
}

#[cfg(unix)]
#[allow(clippy::zombie_processes)]
fn crash_tracking_empty_endpoint_inner(receiver_type: ReceiverType) {
    use std::os::unix::net::UnixListener;

    let (crashtracker_bin, crashtracker_receiver, crashtracker_unix_socket_receiver) =
        setup_crashtracking_crates(BuildProfile::Debug);
    let fixtures = setup_test_fixtures(&[
        &crashtracker_receiver,
        &crashtracker_bin,
        &crashtracker_unix_socket_receiver,
    ]);

    let socket_path = extend_path(fixtures.tmpdir.path(), "trace_agent.socket");
    let listener = UnixListener::bind(&socket_path).unwrap();

    process::Command::new(&fixtures.artifacts[&crashtracker_bin])
        // empty url, endpoint will be set to none
        .arg(receiver_type.to_string())
        .arg("")
        .arg(fixtures.artifacts[&crashtracker_receiver].as_os_str())
        .arg(fixtures.artifacts[&crashtracker_unix_socket_receiver].as_os_str())
        .arg(&fixtures.stderr_path)
        .arg(&fixtures.stdout_path)
        .arg(&fixtures.unix_socket_path)
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
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    unix_socket_path: PathBuf,

    artifacts: HashMap<&'a ArtifactsBuild, PathBuf>,
}

fn setup_test_fixtures<'a>(crates: &[&'a ArtifactsBuild]) -> TestFixtures<'a> {
    let artifacts = build_artifacts(crates).unwrap();

    let tmpdir = tempfile::TempDir::new().unwrap();
    let dirpath = tmpdir.path();
    TestFixtures {
        crash_profile_path: extend_path(dirpath, "crash"),
        crash_telemetry_path: extend_path(dirpath, "crash.telemetry"),
        stdout_path: extend_path(dirpath, "out.stdout"),
        stderr_path: extend_path(dirpath, "out.stderr"),
        unix_socket_path: extend_path(dirpath, "crashtracker.socket"),

        artifacts,
        tmpdir,
    }
}

fn setup_crashtracking_crates(
    crash_tracking_receiver_profile: BuildProfile,
) -> (ArtifactsBuild, ArtifactsBuild, ArtifactsBuild) {
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
    let crashtracker_unix_socket_receiver = ArtifactsBuild {
        name: "crashtracker_unix_socket_receiver".to_owned(),
        build_profile: crash_tracking_receiver_profile,
        artifact_type: ArtifactType::Bin,
        triple_target: None,
    };
    (
        crashtracker_bin,
        crashtracker_receiver,
        crashtracker_unix_socket_receiver,
    )
}

fn extend_path<T: AsRef<Path>>(parent: &Path, path: T) -> PathBuf {
    let mut parent = parent.to_path_buf();
    parent.push(path);
    parent
}
