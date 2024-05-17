// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]
use std::{
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
#[cfg_attr(miri, ignore)]
fn test_spawning_trampoline_worker() {
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();

    let child = unsafe { SpawnWorker::new() }
        .target(Target::ManualTrampoline(
            "__dummy_mirror_test".to_string(),
            "symbol_name".to_string(),
        ))
        .stdin(Stdio::Null)
        .stdout(&stdout)
        .stderr(&stderr)
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
