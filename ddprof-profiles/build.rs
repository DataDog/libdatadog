// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

fn main() -> Result<(), std::io::Error> {
    let protos = &[concat!(env!("CARGO_MANIFEST_DIR"), "/src/profile.proto")];
    let includes = &[concat!(env!("CARGO_MANIFEST_DIR"), "/src")];
    prost_build::compile_protos(protos, includes)?;
    Ok(())
}
