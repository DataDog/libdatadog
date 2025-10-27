// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

use std::{fs, process};

use bin_tests::{build_artifacts, ArtifactType, ArtifactsBuild, BuildProfile};

#[test]
#[cfg_attr(miri, ignore)]
fn test_the_tests_debug() {
    test_the_tests_inner(BuildProfile::Debug);
}

#[test]
#[ignore] // This test is slow, only run it if explicitly opted in
fn test_the_tests_release() {
    test_the_tests_inner(BuildProfile::Release);
}

fn test_the_tests_inner(profile: BuildProfile) {
    let test_the_tests = ArtifactsBuild {
        name: "test_the_tests".to_owned(),
        build_profile: profile,
        artifact_type: ArtifactType::Bin,
        triple_target: None,
    };
    let crates = &[
        &ArtifactsBuild {
            name: "libdd-profiling-ffi".to_owned(),
            build_profile: profile,
            artifact_type: ArtifactType::CDylib,
            triple_target: None,
        },
        &test_the_tests,
    ];
    let artifacts = build_artifacts(crates).unwrap();

    for c in crates {
        assert!(fs::metadata(&artifacts[c]).unwrap().file_type().is_file());
    }

    let mut res = process::Command::new(&artifacts[&test_the_tests])
        .spawn()
        .unwrap();
    assert!(res.wait().unwrap().success());
}
