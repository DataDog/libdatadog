// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(windows)]

use anyhow::Context;
use std::path::PathBuf;
use std::{env, fs, process};

#[test]
fn test_test() {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };

    println!("Profile: {:?}", profile);

    let tmpdir = tempfile::TempDir::new().unwrap();
    let dirpath = tmpdir.path();
    let crash_path = dirpath.join("crash");

    let test_app_path = get_artifact_dir().join(profile).join("test_app.exe");

    println!("Test app path: {:?}", test_app_path);

    let output = process::Command::new(test_app_path)
        .arg(crash_path.to_str().unwrap())
        .output()
        .unwrap();

    println!("Test output: {}", String::from_utf8_lossy(&output.stdout));

    assert!(!output.status.success());

    let crash_report = fs::read(crash_path)
        .context("reading crash report")
        .unwrap();
    let crash_payload = serde_json::from_slice::<serde_json::Value>(&crash_report)
        .context("deserializing crash report to json")
        .unwrap();

    println!("Crash payload: {:?}", crash_payload);

    assert_eq!(&crash_payload["error"]["is_crash"], true);
    assert_eq!(&crash_payload["error"]["kind"], "Panic");
    assert_eq!(&crash_payload["incomplete"], false);
    assert_eq!(&crash_payload["metadata"]["library_name"], "test_library");
    assert_eq!(
        &crash_payload["metadata"]["library_version"],
        "test_version"
    );
    assert_eq!(&crash_payload["metadata"]["family"], "test_family");
}

fn get_artifact_dir() -> PathBuf {
    // This variable contains the path in which cargo puts it's build artifacts
    // This relies on the assumption that the current binary is assumed to not have been moved from
    // its directory
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
}
