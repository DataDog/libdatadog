// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::error::Error;

#[cfg(feature = "generate-protobuf")]
use std::{
    env, fs,
    path::{Path, PathBuf},
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
        // SAFETY: build scripts run single-threaded at build time; no other thread
        // can be concurrently reading or writing the environment.
        unsafe {
            std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path().unwrap());
        }

        let mut config = prost_build::Config::new();

        // Stage prost's output in OUT_DIR instead of letting it write straight into
        // src/: the generated file is checked in, and rewriting it with unchanged
        // content still bumps its mtime, which dirties every feature combination of
        // this crate (and its dependents) on the next build. In CI, the
        // `--all-features` pass regenerates it, and the bin_tests prebuild step that
        // runs right after rebuilds ddsketch -> telemetry -> crashtracker -> bin_tests
        // and half the test artifacts for nothing, twice per pipeline, per platform.
        let staging_dir = PathBuf::from(env::var("OUT_DIR")?);
        config.out_dir(&staging_dir);

        println!("cargo:rerun-if-changed=src/pb/DDSketch.proto");
        config.compile_protos(&["src/pb/DDSketch.proto"], &["src/"])?;

        let generated = [
            HEADER.as_bytes(),
            fs::read(staging_dir.join("pb.rs"))?.as_slice(),
        ]
        .concat();
        let dest = Path::new(&env::var("CARGO_MANIFEST_DIR")?)
            .join("src")
            .join("pb.rs");
        write_if_changed(&dest, &generated)?;
    }
    #[cfg(not(feature = "generate-protobuf"))]
    {
        println!("cargo:rerun-if-changed=build.rs");
    }

    Ok(())
}

/// Writes `contents` to `dest` only when they differ from what's already there, so
/// regenerating an unchanged file leaves its mtime — and cargo's fingerprints for
/// everything downstream — alone. Missing files are written unconditionally.
#[cfg(feature = "generate-protobuf")]
fn write_if_changed(dest: &Path, contents: &[u8]) -> Result<(), Box<dyn Error>> {
    if fs::read(dest)
        .map(|existing| existing == contents)
        .unwrap_or(false)
    {
        return Ok(());
    }
    fs::write(dest, contents)?;
    Ok(())
}
