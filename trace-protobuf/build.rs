use std::env;
use std::fs::File;
use std::io::{Read, Result, Write};
use std::path::Path;

// compiles the .proto files into rust structs
fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=src/pb");
    let mut config = prost_build::Config::new();

    let cur_working_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let current_working_dir_path = Path::new(&cur_working_dir);
    let output_path = current_working_dir_path.join(Path::new("src"));

    config.out_dir(output_path.clone());

    config.message_attribute("Span", "#[derive(Deserialize, Serialize)]");
    config.field_attribute("service", "#[serde(default)]");
    config.field_attribute("parentID", "#[serde(default)]");
    config.field_attribute("error", "#[serde(default)]");
    config.field_attribute("metrics", "#[serde(default)]");
    config.field_attribute("meta_struct", "#[serde(default)]");
    config.field_attribute("type", "#[serde(default)]");

    config.compile_protos(
        &[
            "src/pb/agent_payload.proto",
            "src/pb/tracer_payload.proto",
            "src/pb/span.proto",
        ],
        &["src/pb/"],
    )?;

    // add the serde import to the top of the protobuf rust structs file
    let serde_import = "use serde::{Deserialize, Serialize};

"
    .as_bytes();

    prepend_to_file(serde_import, &output_path.join("pb.rs"));

    // add license to the top of the protobuf rust structs file
    let license = "// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

"
    .as_bytes();

    prepend_to_file(license, &output_path.join("pb.rs"));

    Ok(())
}

fn prepend_to_file(data: &[u8], file_path: &Path) {
    let mut f = File::open(file_path).unwrap();
    let mut content = data.to_owned();
    f.read_to_end(&mut content).unwrap();

    let mut f = File::create(file_path).unwrap();
    f.write_all(content.as_slice()).unwrap();
}
