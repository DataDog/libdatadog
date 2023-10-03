// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
// #![cfg(feature = "prefer-dynamic")]
// use test_spawn_from_lib::spawn_self;

use spawn_worker::Stdio;
use std::io::{Read, Seek};

fn rewind_and_read(file: &mut std::fs::File) -> anyhow::Result<String> {
    file.rewind()?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).unwrap();

    Ok(buf)
}

/// run with: RUSTFLAGS="-C prefer-dynamic" cargo test --package test_spawn_from_lib --features prefer-dynamic -- --ignored
#[test]
#[ignore = "requires -C prefer-dynamic"]
#[cfg(feature = "prefer_dynamic")]
fn test_spawning_trampoline_worker() {
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();

    let child = test_spawn_from_lib::build()
        .stdin(Stdio::Null)
        .stdout(&stdout)
        .stderr(&stderr)
        .spawn()
        .unwrap();

    let status = child.wait().unwrap();

    let stderr = rewind_and_read(&mut stderr).unwrap();
    let stdout = rewind_and_read(&mut stdout).unwrap();

    #[cfg(unix)]
    let success = matches!(status, spawn_worker::WaitStatus::Exited(_, 0));
    #[cfg(windows)]
    let success = status.success();

    if !success {
        eprintln!("{}", stderr);
        panic!("unexpected exit status = {:?}", status)
    }

    assert_eq!("stderr_works_as_expected", stderr.trim());
    assert_eq!("stdout_works_as_expected", stdout.trim());
}
