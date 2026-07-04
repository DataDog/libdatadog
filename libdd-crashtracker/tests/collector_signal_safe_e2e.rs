// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(all(unix, feature = "collector_signal-safe", not(feature = "std")))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use libdd_crashtracker::collector_signal_safe::{
    bootstrap_complete, init_from_env, set_stage, Stage,
};

#[test]
fn signal_safe_child_process() {
    if std::env::var_os("DD_SIGNAL_SAFE_E2E_CHILD").is_none() {
        return;
    }

    assert!(init_from_env());
    bootstrap_complete();
    set_stage(Stage::Application);

    std::process::abort();
}

#[test]
fn signal_safe_crash_writes_report() {
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
        .arg("signal_safe_child_process")
        .arg("--nocapture")
        .env("DD_SIGNAL_SAFE_E2E_CHILD", "1")
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
    assert!(report.contains("DD_CRASHTRACK_BEGIN_CONFIG\n"));
    assert!(report.contains("DD_CRASHTRACK_BEGIN_METADATA\n"));
    assert!(report.contains("\"service:signal-safe-e2e\""));
    assert!(report.contains("DD_CRASHTRACK_BEGIN_SIGINFO\n"));
    assert!(report.contains("\"si_signo_human_readable\":\"SIGABRT\""));
    assert!(report.contains("DD_CRASHTRACK_BEGIN_PROCESSINFO\n"));
    assert!(report.contains("DD_CRASHTRACK_BEGIN_STACKTRACE\n"));
    assert!(report.contains("Crash during application (SIGABRT)"));
    assert!(report.ends_with("DD_CRASHTRACK_DONE\n"));
}
