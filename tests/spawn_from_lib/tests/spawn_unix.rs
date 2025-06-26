// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

use std::{
    fs::File,
    io::{Read, Seek},
};

use spawn_worker::{Stdio, WaitStatus};
use test_spawn_from_lib::build;

fn rewind_and_read(file: &mut File) -> anyhow::Result<String> {
    file.rewind()?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).unwrap();

    Ok(buf)
}

/// to test the FdExec/Exec trampolining
/// additionally run: RUSTFLAGS="-C prefer-dynamic" cargo test --package tests/spawn_from_lib
#[test]
#[cfg_attr(miri, ignore)]
fn test_spawning_trampoline_worker() {
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();
    let mut shared_file = tempfile::tempfile().unwrap();

    let child = build()
        .stdin(Stdio::Null)
        .stdout(&stdout)
        .stderr(&stderr)
        .pass_fd(shared_file.try_clone().unwrap())
        .spawn()
        .unwrap();

    let code = match child.wait().unwrap() {
        WaitStatus::Exited(_, s) => s,
        _ => unreachable!("shouldn't happen"),
    };

    let stderr = rewind_and_read(&mut stderr).unwrap();
    let stdout = rewind_and_read(&mut stdout).unwrap();
    let shared_file = rewind_and_read(&mut shared_file).unwrap();

    if code != 0 {
        eprintln!("{stderr}");
        println!("{stdout}");

        assert_eq!(0, code, "non zero exit code");
    }

    assert_eq!("stderr_works_as_expected", stderr.trim());
    assert_eq!("stdout_works_as_expected", stdout.trim());
    assert_eq!("shared_file_works_as_expected", shared_file.trim());
}
