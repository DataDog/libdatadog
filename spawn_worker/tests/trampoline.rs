// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
use std::{fs::File, process::Stdio};

use spawn_worker::{SpawnCfg, Target};

#[test]
fn test_spawning_trampoline_worker() {
    let stdout = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    let stderr = tempfile::NamedTempFile::new().unwrap().into_temp_path();

    let child = SpawnCfg::new()
        .target(Target::ManualTrampoline(
            String::from("__dummy_mirror_test"),
            String::from("symbol_name"),
        ))
        .stdin(Stdio::null())
        .stdout(File::open(stdout).unwrap())
        .stderr(File::open(stderr).unwrap())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    //wait for process exit
    let output = child.wait_with_output().unwrap();

    // let stderr = child.stderr.unwrap();
    if !output.status.success() {
        eprintln!("{}", String::from_utf8_lossy(output.stderr.as_slice()));
        panic!("unexpected exit status = {:?}", output.status)
    }

    assert_eq!(
        "__dummy_mirror_test symbol_name",
        String::from_utf8(output.stdout).unwrap()
    );
}
