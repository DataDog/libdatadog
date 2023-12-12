// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use std::{fs, process};

use bin_tests::{build_artifacts, ArtifactType, ArtifactsBuild, Profile};

#[test]
#[cfg_attr(miri, ignore)]
fn test_the_tests_debug() {
    test_the_tests_inner(Profile::Debug);
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_the_tests_release() {
    test_the_tests_inner(Profile::Release);
}

fn test_the_tests_inner(profile: Profile) {
    let test_the_tests = ArtifactsBuild {
        name: "test_the_tests".to_owned(),
        profile: profile,
        artifact_type: ArtifactType::Bin,
        triple_target: None,
    };
    let crates = &[
        &ArtifactsBuild {
            name: "datadog-profiling-ffi".to_owned(),
            profile: profile,
            artifact_type: ArtifactType::CDylib,
            triple_target: None,
        },
        &ArtifactsBuild {
            name: "profiling-crashtracking-receiver".to_owned(),
            profile: profile,
            artifact_type: ArtifactType::ExecutablePackage,
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
