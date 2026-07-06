// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(all(unix, feature = "collector_signal-safe"))]

use std::fs;
use std::os::fd::AsRawFd;
#[cfg(any(target_os = "linux", target_os = "android"))]
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[cfg(any(target_os = "linux", target_os = "android"))]
use libdd_crashtracker::collector_signal_safe::init_from_env_result;
use libdd_crashtracker::collector_signal_safe::{
    bootstrap_complete, init_result, owned_signal_count, owns_signal, set_stage, InitResult,
    SignalSafeInitConfig, Stage,
};

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn signal_safe_receiver_child_process() {
    if std::env::var_os("DD_SIGNAL_SAFE_E2E_RECEIVER_CHILD").is_none() {
        return;
    }

    assert_eq!(init_from_env_result(), InitResult::Enabled);
    bootstrap_complete();
    set_stage(Stage::Application);

    std::process::abort();
}

#[test]
fn signal_safe_report_fd_child_process() {
    let Some(report) = std::env::var_os("DD_SIGNAL_SAFE_E2E_REPORT_FD_CHILD") else {
        return;
    };

    let report = fs::File::create(report).expect("create report");
    assert_eq!(
        init_result(&SignalSafeInitConfig {
            receiver_path: b"/definitely/missing-signal-safe-receiver",
            service: b"signal-safe-e2e",
            env: b"test",
            app_version: b"1",
            runtime_id: b"00000000-0000-0000-0000-000000000001",
            report_fd: report.as_raw_fd(),
            ..SignalSafeInitConfig::default()
        }),
        InitResult::Enabled
    );
    bootstrap_complete();
    set_stage(Stage::Application);

    std::process::abort();
}

#[test]
fn signal_safe_stage_child_process() {
    let Some(report) = std::env::var_os("DD_SIGNAL_SAFE_E2E_STAGE_CHILD") else {
        return;
    };
    let stage = std::env::var("DD_SIGNAL_SAFE_E2E_STAGE").expect("stage");

    let _report = init_report_fd(report, b"/definitely/missing-signal-safe-receiver", false);
    if stage == "application" {
        bootstrap_complete();
    }
    std::process::abort();
}

#[test]
fn signal_safe_bootstrap_only_child_process() {
    let Some(report) = std::env::var_os("DD_SIGNAL_SAFE_E2E_BOOTSTRAP_ONLY_CHILD") else {
        return;
    };

    let _report = init_report_fd(report, b"/definitely/missing-signal-safe-receiver", true);
    bootstrap_complete();
    std::process::abort();
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn signal_safe_receiver_deleted_child_process() {
    let Some(report) = std::env::var_os("DD_SIGNAL_SAFE_E2E_RECEIVER_DELETED_CHILD") else {
        return;
    };
    let receiver = std::env::var_os("DD_SIGNAL_SAFE_E2E_RECEIVER").expect("receiver");

    let _report = init_report_fd(report, receiver.as_encoded_bytes(), false);
    bootstrap_complete();
    set_stage(Stage::Application);
    fs::remove_file(receiver).expect("remove receiver");
    std::process::abort();
}

#[test]
fn signal_safe_preexisting_app_handler_child_process() {
    let Some(report) = std::env::var_os("DD_SIGNAL_SAFE_E2E_APP_HANDLER_CHILD") else {
        return;
    };

    install_noop_handler(libc::SIGSEGV);
    let _report = init_report_fd(report, b"/definitely/missing-signal-safe-receiver", false);
    assert!(!owns_signal(libc::SIGSEGV));
    assert!(owns_signal(libc::SIGABRT));
    assert!(owned_signal_count() < 5);
    bootstrap_complete();
    set_stage(Stage::Application);
    std::process::abort();
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn signal_safe_crash_writes_report_through_receiver() {
    let temp = tempfile::tempdir().expect("tempdir");
    let receiver = temp.path().join("receiver.sh");
    let report = temp.path().join("report.txt");

    fs::write(
        &receiver,
        b"#!/bin/sh\ncat > \"$DD_SIGNAL_SAFE_E2E_REPORT\"\n",
    )
    .expect("write receiver");
    let mut perms = fs::metadata(&receiver).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&receiver, perms).expect("chmod receiver");

    let current_exe = std::env::current_exe().expect("current_exe");
    let status = Command::new(current_exe)
        .arg("--exact")
        .arg("signal_safe_receiver_child_process")
        .arg("--nocapture")
        .env("DD_SIGNAL_SAFE_E2E_RECEIVER_CHILD", "1")
        .env("DD_SIGNAL_SAFE_E2E_REPORT", &report)
        .env("DD_TRACE_C_CRASHTRACKER_PROCESS", &receiver)
        .env("DD_SERVICE", "signal-safe-e2e")
        .env("DD_ENV", "test")
        .env("DD_VERSION", "1")
        .env("DD_RUNTIME_ID", "00000000-0000-0000-0000-000000000001")
        .status()
        .expect("spawn child");

    assert!(!status.success(), "child should terminate via signal");

    let report = fs::read_to_string(&report).expect("read crash report");
    assert_common_report_shape(&report);
    assert!(report.contains("\"si_signo_human_readable\":\"SIGABRT\""));
}

#[test]
fn signal_safe_crash_writes_report_to_fd_when_degraded() {
    let temp = tempfile::tempdir().expect("tempdir");
    let report = temp.path().join("report.txt");

    let current_exe = std::env::current_exe().expect("current_exe");
    let status = Command::new(current_exe)
        .arg("--exact")
        .arg("signal_safe_report_fd_child_process")
        .arg("--nocapture")
        .env("DD_SIGNAL_SAFE_E2E_REPORT_FD_CHILD", &report)
        .status()
        .expect("spawn child");

    assert!(!status.success(), "child should terminate via signal");

    let report = fs::read_to_string(&report).expect("read crash report");
    assert_common_report_shape(&report);
    assert!(report.contains("\"si_signo_human_readable\":\"SIGABRT\""));
    assert!(report.contains("\"report_degraded:missing_receiver\""));
    assert!(report.contains("\"report_degraded:report_to_fd\""));
}

#[test]
fn signal_safe_stage_tags_track_bootstrap_completion() {
    let init_report = run_stage_child("crashtracker_init");
    assert!(init_report.contains("\"stage:crashtracker_init\""));

    let application_report = run_stage_child("application");
    assert!(application_report.contains("\"stage:application\""));
}

#[test]
fn signal_safe_bootstrap_only_shutdown_suppresses_later_report() {
    let temp = tempfile::tempdir().expect("tempdir");
    let report = temp.path().join("report.txt");

    let current_exe = std::env::current_exe().expect("current_exe");
    let status = Command::new(current_exe)
        .arg("--exact")
        .arg("signal_safe_bootstrap_only_child_process")
        .arg("--nocapture")
        .env("DD_SIGNAL_SAFE_E2E_BOOTSTRAP_ONLY_CHILD", &report)
        .status()
        .expect("spawn child");

    assert!(!status.success(), "child should terminate via signal");
    let contents = fs::read_to_string(&report).unwrap_or_default();
    assert!(contents.is_empty(), "bootstrap-only mode should not emit");
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn signal_safe_receiver_deleted_after_init_falls_back_to_report_fd() {
    let temp = tempfile::tempdir().expect("tempdir");
    let receiver = temp.path().join("receiver.sh");
    let report = temp.path().join("report.txt");

    fs::write(&receiver, b"#!/bin/sh\ncat >/dev/null\n").expect("write receiver");
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let mut perms = fs::metadata(&receiver).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&receiver, perms).expect("chmod receiver");
    }

    let current_exe = std::env::current_exe().expect("current_exe");
    let status = Command::new(current_exe)
        .arg("--exact")
        .arg("signal_safe_receiver_deleted_child_process")
        .arg("--nocapture")
        .env("DD_SIGNAL_SAFE_E2E_RECEIVER_DELETED_CHILD", &report)
        .env("DD_SIGNAL_SAFE_E2E_RECEIVER", &receiver)
        .status()
        .expect("spawn child");

    assert!(!status.success(), "child should terminate via signal");
    let report = fs::read_to_string(&report).expect("read crash report");
    assert_common_report_shape(&report);
    assert!(report.contains("\"report_degraded:receiver_unavailable\""));
    assert!(report.contains("\"report_degraded:report_to_fd\""));
}

#[test]
fn signal_safe_preexisting_app_handler_is_reported_without_internal_state_setup() {
    let temp = tempfile::tempdir().expect("tempdir");
    let report = temp.path().join("report.txt");

    let current_exe = std::env::current_exe().expect("current_exe");
    let status = Command::new(current_exe)
        .arg("--exact")
        .arg("signal_safe_preexisting_app_handler_child_process")
        .arg("--nocapture")
        .env("DD_SIGNAL_SAFE_E2E_APP_HANDLER_CHILD", &report)
        .status()
        .expect("spawn child");

    assert!(!status.success(), "child should terminate via signal");
    let report = fs::read_to_string(&report).expect("read crash report");
    assert_common_report_shape(&report);
    assert!(report.contains("\"report_degraded:app_handler_present\""));
    assert!(report.contains(&format!(
        "\"report_degraded:app_handler_present:{}\"",
        libc::SIGSEGV
    )));
}

fn run_stage_child(stage: &str) -> String {
    let temp = tempfile::tempdir().expect("tempdir");
    let report = temp.path().join("report.txt");

    let current_exe = std::env::current_exe().expect("current_exe");
    let status = Command::new(current_exe)
        .arg("--exact")
        .arg("signal_safe_stage_child_process")
        .arg("--nocapture")
        .env("DD_SIGNAL_SAFE_E2E_STAGE_CHILD", &report)
        .env("DD_SIGNAL_SAFE_E2E_STAGE", stage)
        .status()
        .expect("spawn child");

    assert!(!status.success(), "child should terminate via signal");
    fs::read_to_string(&report).expect("read crash report")
}

fn init_report_fd(
    report_path: impl AsRef<std::path::Path>,
    receiver_path: &[u8],
    only_bootstrap: bool,
) -> fs::File {
    let report = fs::File::create(report_path).expect("create report");
    assert_eq!(
        init_result(&SignalSafeInitConfig {
            receiver_path,
            service: b"signal-safe-e2e",
            env: b"test",
            app_version: b"1",
            runtime_id: b"00000000-0000-0000-0000-000000000001",
            report_fd: report.as_raw_fd(),
            only_bootstrap,
            ..SignalSafeInitConfig::default()
        }),
        InitResult::Enabled
    );
    report
}

extern "C" fn noop_handler(_: libc::c_int) {}

fn install_noop_handler(signal: libc::c_int) {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = noop_handler as *const () as usize;
    action.sa_flags = 0;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
        assert_eq!(libc::sigaction(signal, &action, std::ptr::null_mut()), 0);
    }
}

fn assert_common_report_shape(report: &str) {
    assert!(report.contains("DD_CRASHTRACK_BEGIN_CONFIG\n"));
    assert!(report.contains("DD_CRASHTRACK_BEGIN_METADATA\n"));
    assert!(report.contains("\"service:signal-safe-e2e\""));
    assert!(report.contains("DD_CRASHTRACK_BEGIN_SIGINFO\n"));
    assert!(report.contains("DD_CRASHTRACK_BEGIN_PROCESSINFO\n"));
    assert!(report.contains("DD_CRASHTRACK_BEGIN_STACKTRACE\n"));
    assert!(report.ends_with("DD_CRASHTRACK_DONE\n"));
}
