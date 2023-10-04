use std::error::Error;

#[cfg(feature = "generate-protobuf")]
use std::{
    env,
    fs::File,
    io::{Read, Write},
    path::Path,
};

#[cfg(feature = "generate-protobuf")]
const HEADER: &str =
    "// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

// This file has been automatically generated from build.rs

";

fn main() -> Result<(), Box<dyn Error>> {
    #[cfg(feature = "generate-protobuf")]
    {
        let mut config = prost_build::Config::new();

        let cur_working_dir = env::var("CARGO_MANIFEST_DIR")?;
        let output_path = Path::new(&cur_working_dir).join("src");

        config.out_dir(output_path.clone());

        println!("cargo:rerun-if-changed=src/pb/DDSketch.proto");
        config.compile_protos(&["src/pb/DDSketch.proto"], &["src/"])?;

        prepend_to_file(HEADER.as_bytes(), &output_path.join("pb.rs"))?;
    }

    Ok(())
}

#[cfg(feature = "generate-protobuf")]
fn prepend_to_file(data: &[u8], file_path: &Path) -> Result<(), Box<dyn Error>> {
    let mut f = File::open(file_path)?;
    let mut content = data.to_owned();
    f.read_to_end(&mut content)?;

    let mut f = File::create(file_path)?;
    f.write_all(content.as_slice())?;
    Ok(())
}
