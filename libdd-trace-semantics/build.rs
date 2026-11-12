// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Compiles `mappings.json` into constant data. An invalid file fails the build.

use core::error::Error;
use std::path::PathBuf;

#[path = "codegen.rs"]
mod codegen;

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=mappings.json");
    println!("cargo:rerun-if-changed=codegen.rs");

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let json = std::fs::read_to_string(manifest_dir.join("mappings.json"))?;
    let registry = codegen::parse(&json)?;

    let out = PathBuf::from(std::env::var("OUT_DIR")?).join("registry.rs");
    std::fs::write(out, codegen::generate(&registry))?;
    Ok(())
}
