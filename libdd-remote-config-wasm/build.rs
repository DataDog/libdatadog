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
    println!("cargo:rerun-if-changed=native.h.in");
    let abi: Abi = serde_json::from_str(include_str!("abi.json"))?;
    let path = PathBuf::from(env::var_os("OUT_DIR").ok_or("missing OUT_DIR")?);
    let target_os = env::var("CARGO_CFG_TARGET_OS")?;
    match target_os.as_str() {
        "macos" => println!(
            "cargo:rustc-link-arg-cdylib=-Wl,-install_name,@rpath/liblibdd_remote_config_wasm.dylib"
        ),
        "linux" => {
            println!("cargo:rustc-link-arg-cdylib=-Wl,-soname,liblibdd_remote_config_wasm.so")
        }
        _ => {}
    }
    let (output, header) = if target_os == "wasi" {
        (wasi_exports(&abi)?, None)
    } else {
        let (output, header) = native_exports(&abi)?;
        (output, Some(header))
    };
    fs::write(path.join("exports.rs"), output)?;
    if let Some(header) = header {
        fs::write(path.join("ddog_remote_config.h"), header)?;
    }
    Ok(())
}

fn wasi_exports(abi: &Abi) -> Result<String, std::fmt::Error> {
    let mut output = format!(
        "pub const ABI_VERSION: i32 = {};\n\
         std::thread_local! {{ static REGISTRY: std::cell::RefCell<crate::api::Registry> = Default::default(); }}\n\
         #[unsafe(no_mangle)] pub extern \"C\" fn rc_version() -> i32 {{ ABI_VERSION }}\n",
        abi.version
    );
    for (name, arguments) in &abi.exports {
        if name == "rc_version" {
            continue;
        }
        let parameters = arguments
            .iter()
            .map(|argument| format!("{argument}: i32"))
            .collect::<Vec<_>>()
            .join(", ");
        let operation = registry_operation(name, arguments);
        writeln!(
            output,
            "#[unsafe(no_mangle)] pub extern \"C\" fn {name}({parameters}) -> i32 {{ REGISTRY.with_borrow_mut({operation}) }}"
        )?;
    }
    Ok(output)
}

fn native_exports(abi: &Abi) -> Result<(String, String), std::fmt::Error> {
    let mut output = String::new();
    let mut declarations = String::new();
    for (name, arguments) in &abi.exports {
        if matches!(name.as_str(), "rc_version" | "rc_buffer_ptr") {
            continue;
        }
        let parameters = with_leading_comma(
            arguments
                .iter()
                .map(|argument| format!("{argument}: i32"))
                .collect::<Vec<_>>()
                .join(", "),
        );
        let c_parameters = with_leading_comma(
            arguments
                .iter()
                .map(|argument| format!("int32_t {argument}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
        let operation = registry_operation(name, arguments);
        writeln!(
            output,
            "/// # Safety\n/// A non-null reactor must be live and have no outstanding buffer borrow.\n\
             #[unsafe(no_mangle)] pub unsafe extern \"C\" fn ddog_{name}(reactor: *mut crate::native::Reactor{parameters}) -> i32 {{\n\
             crate::native::catch_panic!(-1, || unsafe {{ crate::native::with_registry(reactor, {operation}) }})\n}}"
        )?;
        writeln!(
            declarations,
            "int32_t ddog_{name}(ddog_rc_reactor *reactor{c_parameters});"
        )?;
    }
    let header = include_str!("native.h.in").replace("@DECLARATIONS@", &declarations);
    Ok((output, header))
}

fn registry_operation(name: &str, arguments: &[String]) -> String {
    if arguments.is_empty() {
        format!("crate::api::{name}")
    } else {
        let args = arguments.join(", ");
        format!("|registry| crate::api::{name}(registry, {args})")
    }
}

fn with_leading_comma(value: String) -> String {
    if value.is_empty() {
        value
    } else {
        format!(", {value}")
    }
}
