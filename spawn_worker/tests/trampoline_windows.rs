// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(windows)]
use std::{fs, fs::OpenOptions};

use spawn_worker::{SpawnWorker, Stdio, Target};

#[test]
fn test_spawning_trampoline_worker() {
    let stdout = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    let stderr = tempfile::NamedTempFile::new().unwrap().into_temp_path();

    let status = SpawnWorker::new()
        .target(Target::ManualTrampoline(
            String::from("__dummy_mirror_test"),
            String::from("symbol_name"),
        ))
        .stdin(Stdio::Null)
        .stdout(
            &OpenOptions::new()
                .read(true)
                .write(true)
                .open(&stdout)
                .unwrap(),
        )
        .stderr(
            &OpenOptions::new()
                .read(true)
                .write(true)
                .open(&stderr)
                .unwrap(),
        )
        .spawn()
        .unwrap()
        .wait()
        .unwrap();

    //wait for process exit
    let output = fs::read_to_string(stdout.as_os_str()).unwrap();

    if !status.success() {
        eprintln!("{}", fs::read_to_string(stderr.as_os_str()).unwrap());
        panic!("unexpected exit status = {status:?}")
    }

    assert_eq!("__dummy_mirror_test symbol_name", output);
}
