// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
// #![cfg(feature = "prefer-dynamic")]
// use test_spawn_from_lib::spawn_self;

use std::{
    io::{Read, Seek},
    process::Stdio,
};

fn rewind_and_read(file: &mut std::fs::File) -> anyhow::Result<String> {
    file.rewind()?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).unwrap();

    Ok(buf)
}

/// run with: RUSTFLAGS="-C prefer-dynamic" cargo test --package test_spawn_from_lib --features prefer-dynamic -- --ignored
#[test]
#[ignore = "requires -C prefer-dynamic"]
fn test_spawning_trampoline_worker() {
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();

    let child = test_spawn_from_lib::build()
        .stdin(Stdio::null())
        .stdout(stdout.try_clone().unwrap())
        .stderr(stderr.try_clone().unwrap())
        .spawn()
        .unwrap();

    let output = child.wait_with_output().unwrap();

    if !output.status.success() {
        eprintln!("{}", String::from_utf8_lossy(output.stderr.as_slice()));
        panic!("unexpected exit status = {:?}", output.status)
    }

    let stderr = rewind_and_read(&mut stderr).unwrap();
    let stdout = rewind_and_read(&mut stdout).unwrap();

    assert_eq!(Some(0), output.status.code());

    assert_eq!("stderr_works_as_expected", stderr.trim());
    assert_eq!("stdout_works_as_expected", stdout.trim());
}
