// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
#![cfg(unix)]
use std::{
    ffi::CString,
    fs::File,
    io::{Read, Seek},
};

use nix::sys::wait::WaitStatus;
use spawn_worker::{SpawnWorker, Stdio, Target};

fn rewind_and_read(file: &mut File) -> anyhow::Result<String> {
    file.rewind()?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).unwrap();
    Ok(buf)
}

#[test]
fn test_spawning_trampoline_worker() {
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();

    let child = unsafe { SpawnWorker::new() }
        .target(Target::Manual(
            CString::new("__dummy_mirror_test").unwrap(),
            CString::new("symbol_name").unwrap(),
        ))
        .stdin(Stdio::Null)
        .stdout(stdout.try_clone().unwrap())
        .stderr(stderr.try_clone().unwrap())
        .spawn()
        .unwrap();

    //wait for process exit
    let status = child.wait().unwrap();

    match status {
        WaitStatus::Exited(_, s) => assert_eq!(0, s),

        others => {
            eprintln!("{}", rewind_and_read(&mut stderr).unwrap());
            panic!("unexpected exit status = {others:?}")
        }
    }

    assert_eq!(
        "__dummy_mirror_test symbol_name",
        rewind_and_read(&mut stdout).unwrap()
    );
}
