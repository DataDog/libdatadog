// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::error::Error;

#[cfg(feature = "generate-protobuf")]
use std::{
    env,
    fs::File,
    io::{Read, Write},
    path::Path,
};

#[cfg(feature = "generate-protobuf")]
const HEADER: &str = "// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

";

fn main() -> Result<(), Box<dyn Error>> {
    #[cfg(feature = "generate-protobuf")]
    {
        // protoc is required to compile proto files. This uses protobuf_src to compile protoc
        // from the source, setting the env var to tell prost_build where to find it.
        std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path().unwrap());

        let mut config = prost_build::Config::new();

        let cur_working_dir = env::var("CARGO_MANIFEST_DIR")?;
        let output_path = Path::new(&cur_working_dir).join("src");

        config.out_dir(output_path.clone());

        println!("cargo:rerun-if-changed=src/pb/DDSketch.proto");
        config.compile_protos(&["src/pb/DDSketch.proto"], &["src/"])?;

        prepend_to_file(HEADER.as_bytes(), &output_path.join("pb.rs"))?;
    }
    #[cfg(not(feature = "generate-protobuf"))]
    {
        println!("cargo:rerun-if-changed=build.rs");
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
