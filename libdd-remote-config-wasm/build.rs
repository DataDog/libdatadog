// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::Deserialize;
use std::collections::BTreeMap;
use std::env;
use std::fmt::Write;
use std::fs;
use std::path::PathBuf;

#[derive(Deserialize)]
struct Abi {
    version: i32,
    exports: BTreeMap<String, Vec<String>>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=abi.json");
    if env::var("CARGO_CFG_TARGET_OS")? != "wasi" {
        return Ok(());
    }
    let abi: Abi = serde_json::from_str(include_str!("abi.json"))?;
    let mut output = format!("pub const ABI_VERSION: i32 = {};\n", abi.version);
    for (name, arguments) in abi.exports {
        let parameters = arguments
            .iter()
            .map(|argument| format!("{argument}: i32"))
            .collect::<Vec<_>>()
            .join(", ");
        let args = arguments.join(", ");
        writeln!(
            output,
            "#[unsafe(no_mangle)] pub extern \"C\" fn {name}({parameters}) -> i32 {{ crate::api::{name}({args}) }}"
        )?;
    }
    let path = PathBuf::from(env::var_os("OUT_DIR").ok_or("missing OUT_DIR")?);
    fs::write(path.join("exports.rs"), output)?;
    Ok(())
}
