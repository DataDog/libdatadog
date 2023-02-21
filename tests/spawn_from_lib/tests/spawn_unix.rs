// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
#![cfg(unix)]
#![cfg(feature = "prefer-dynamic")]

use std::{
    fs::File,
    io::{Read, Seek},
};

use io_lifetimes::OwnedFd;
use spawn_worker::{
    spawn::{SpawnCfg, Target},
    WaitStatus,
};
use test_spawn_from_lib::exported_entrypoint;

fn rewind_and_read_fd(fd: OwnedFd) -> anyhow::Result<String> {
    let mut file = File::try_from(fd)?;
    file.rewind()?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).unwrap();

    Ok(buf)
}

/// run with: RUSTFLAGS="-C prefer-dynamic" cargo test --package tests/spawn_from_lib --features prefer-dynamic -- --ignored
#[test]
#[ignore = "requires -C prefer-dynamic"]
fn test_spawning_trampoline_worker() {
    let stdout = tempfile::tempfile().unwrap();
    let stderr = tempfile::tempfile().unwrap();

    let mut child = unsafe { SpawnCfg::new() }
        .target(Target::ViaFnPtr(exported_entrypoint))
        .stdin(File::open("/dev/null").unwrap())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .unwrap();

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    match child.wait().unwrap() {
        WaitStatus::Exited(_, s) => assert_eq!(0, s),
        _ => unreachable!("shouldn't happen"),
    }

    assert_eq!(
        "stderr_works_as_expected",
        rewind_and_read_fd(stderr).unwrap()
    );
    assert_eq!(
        "stdout_works_as_expected",
        rewind_and_read_fd(stdout).unwrap()
    );
}
