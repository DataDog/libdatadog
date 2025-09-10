// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::env;
use std::path::PathBuf;

fn main() {
    // protoc is required to compile proto files. This uses protoc-bin-vendored to provide
    // the protoc binary, setting the env var to tell prost_build where to find it.
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path().unwrap());

    // Tell Cargo to rerun this build script if the proto files change
    println!("cargo:rerun-if-changed=profiles.proto");
    println!("cargo:rerun-if-changed=opentelemetry/proto/common/v1/common.proto");
    println!("cargo:rerun-if-changed=opentelemetry/proto/resource/v1/resource.proto");

    // Create the output directory
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Configure prost-build
    let mut config = prost_build::Config::new();
    config.out_dir(&out_dir);

    // Compile the proto files - include all proto files so imports work correctly
    config
        .compile_protos(
            &[
                "opentelemetry/proto/common/v1/common.proto",
                "opentelemetry/proto/resource/v1/resource.proto",
                "profiles.proto",
            ],
            &["."],
        )
        .expect("Failed to compile protobuf files");
}
