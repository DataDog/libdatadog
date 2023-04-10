use std::io::Result;

#[cfg(feature = "generate-protobuf")]
use {
    std::env,
    std::fs::File,
    std::io::{Read, Write},
    std::path::Path,
};

// to re-generate protobuf structs, run cargo build --features generate-protobuf
fn main() -> Result<()> {
    #[cfg(feature = "generate-protobuf")]
    {
        // protoc is required to compile proto files. This uses protobuf_src to compile protoc
        // from the source, setting the env var to tell prost_build where to find it.
        std::env::set_var("PROTOC", protobuf_src::protoc());

        // compiles the .proto files into rust structs
        generate_protobuf();
    }
    Ok(())
}

#[cfg(feature = "generate-protobuf")]
fn generate_protobuf() {
    let mut config = prost_build::Config::new();

    let cur_working_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let current_working_dir_path = Path::new(&cur_working_dir);
    let output_path = current_working_dir_path.join(Path::new("src"));

    config.out_dir(output_path.clone());

    config.type_attribute("Span", "#[derive(Deserialize, Serialize)]");
    config.field_attribute("service", "#[serde(default)]");
    config.field_attribute("parentID", "#[serde(default)]");
    config.field_attribute("error", "#[serde(default)]");
    config.field_attribute("metrics", "#[serde(default)]");
    config.field_attribute("meta_struct", "#[serde(default)]");
    config.field_attribute("type", "#[serde(default)]");

    config
        .compile_protos(
            &[
                "src/pb/agent_payload.proto",
                "src/pb/tracer_payload.proto",
                "src/pb/span.proto",
            ],
            &["src/pb/"],
        )
        .unwrap();

    // add license & serde imports to the top of the protobuf rust structs file
    let license = "// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

use serde::{Deserialize, Serialize};

"
    .as_bytes();

    prepend_to_file(license, &output_path.join("pb.rs"));
}

#[cfg(feature = "generate-protobuf")]
fn prepend_to_file(data: &[u8], file_path: &Path) {
    let mut f = File::open(file_path).unwrap();
    let mut content = data.to_owned();
    f.read_to_end(&mut content).unwrap();

    let mut f = File::create(file_path).unwrap();
    f.write_all(content.as_slice()).unwrap();
}
