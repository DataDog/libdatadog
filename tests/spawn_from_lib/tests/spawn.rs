// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
// #![cfg(feature = "prefer-dynamic")]
use test_spawn_from_lib::spawn_self;

/// run with: RUSTFLAGS="-C prefer-dynamic" cargo test --package test_spawn_from_lib --features prefer-dynamic -- --ignored
#[test]
#[ignore = "requires -C prefer-dynamic"]
fn test_spawning_trampoline_worker() {
    let child = spawn_self().unwrap();

    let output = child.wait_with_output().unwrap();

    if !output.status.success() {
        eprintln!("{}", String::from_utf8(output.stderr.to_vec()).unwrap());
    }

    assert_eq!(Some(0), output.status.code());

    assert_eq!(
        "stdout_works_as_expected",
        String::from_utf8(output.stdout).unwrap()
    );

    assert_eq!(
        "stderr_works_as_expected",
        String::from_utf8(output.stderr).unwrap()
    );
}
