// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared artifact definitions for bin_tests.
//!
//! This module contains all artifact configurations used by tests and the prebuild binary.

use crate::{ArtifactType, ArtifactsBuild, BuildProfile};

/// Creates an ArtifactsBuild for the crashtracker receiver binary.
pub fn crashtracker_receiver(profile: BuildProfile) -> ArtifactsBuild {
    ArtifactsBuild {
        name: "test_crashtracker_receiver".to_owned(),
        build_profile: profile,
        artifact_type: ArtifactType::Bin,
        ..Default::default()
    }
}

/// Creates an ArtifactsBuild for the crashtracker_bin_test binary.
pub fn crashtracker_bin_test(profile: BuildProfile, panic_abort: bool) -> ArtifactsBuild {
    ArtifactsBuild {
        name: "crashtracker_bin_test".to_owned(),
        build_profile: profile,
        artifact_type: ArtifactType::Bin,
        panic_abort: if panic_abort { Some(true) } else { None },
        ..Default::default()
    }
}

/// Creates an ArtifactsBuild for the crashing_test_app binary.
#[cfg(not(target_os = "macos"))]
pub fn crashing_app(profile: BuildProfile, panic_abort: bool) -> ArtifactsBuild {
    ArtifactsBuild {
        name: "crashing_test_app".to_owned(),
        build_profile: profile,
        artifact_type: ArtifactType::Bin,
        panic_abort: if panic_abort { Some(true) } else { None },
        ..Default::default()
    }
}

/// Creates an ArtifactsBuild for the test_the_tests binary.
pub fn test_the_tests(profile: BuildProfile) -> ArtifactsBuild {
    ArtifactsBuild {
        name: "test_the_tests".to_owned(),
        build_profile: profile,
        artifact_type: ArtifactType::Bin,
        ..Default::default()
    }
}

/// Creates an ArtifactsBuild for the libdd-profiling-ffi CDylib.
pub fn profiling_ffi(profile: BuildProfile) -> ArtifactsBuild {
    ArtifactsBuild {
        name: "libdd-profiling-ffi".to_owned(),
        lib_name_override: Some("datadog_profiling_ffi".to_owned()),
        build_profile: profile,
        artifact_type: ArtifactType::CDylib,
        ..Default::default()
    }
}

/// Standard artifacts used in most crash tracking tests.
pub struct StandardArtifacts {
    pub crashtracker_bin: ArtifactsBuild,
    pub crashtracker_receiver: ArtifactsBuild,
}

impl StandardArtifacts {
    pub fn new(profile: BuildProfile) -> Self {
        Self {
            crashtracker_bin: crashtracker_bin_test(profile, false),
            crashtracker_receiver: crashtracker_receiver(profile),
        }
    }

    pub fn as_slice(&self) -> Vec<&ArtifactsBuild> {
        vec![&self.crashtracker_bin, &self.crashtracker_receiver]
    }
}

/// Returns all artifacts that should be pre-built for bin_tests.
pub fn all_prebuild_artifacts() -> Vec<ArtifactsBuild> {
    let mut artifacts = Vec::new();

    // Standard artifacts for both Debug and Release profiles
    for profile in [BuildProfile::Debug, BuildProfile::Release] {
        artifacts.push(crashtracker_bin_test(profile, false));
        artifacts.push(crashtracker_receiver(profile));
        artifacts.push(test_the_tests(profile));
        artifacts.push(profiling_ffi(profile));

        #[cfg(not(target_os = "macos"))]
        artifacts.push(crashing_app(profile, false));
    }

    // Panic abort variants (used by panic hook tests)
    artifacts.push(crashtracker_bin_test(BuildProfile::Debug, true));

    #[cfg(not(target_os = "macos"))]
    artifacts.push(crashing_app(BuildProfile::Debug, true));

    artifacts
}
