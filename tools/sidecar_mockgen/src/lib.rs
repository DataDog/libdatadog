// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::collections::HashSet;
use object::{File, Object, ObjectSymbol, SymbolKind};
use std::{fs, io};
use std::fmt::Write;
use std::path::Path;

fn check_and_parse<'a>(path: &'a Path, bin_data: &'a io::Result<Vec<u8>>) -> Result<File<&'a [u8]>, String> {
    let bin_data = match bin_data {
        Err(e) => {
            return Err(format!("Could not read {}: {}", path.to_string_lossy(), e));
        }
        Ok(bin_data) => bin_data
    };
    match File::parse(bin_data.as_slice()) {
        Err(e) => {
            return Err(format!("Could not parse {}: {}", path.to_string_lossy(), e));
        }
        Ok(parsed) => Ok(parsed)
    }
}

/// args: first a shared object or executable file, then object files it is to be diffed against
pub fn generate_mock_symbols(binary: &Path, objects: &[&Path]) -> Result<String, String> {
    let mut missing_symbols = HashSet::new();

    let bin_data = fs::read(binary);
    let so_file = check_and_parse(binary, &bin_data)?;

    for path in objects {
        let bin_data = fs::read(path);
        let obj_file = check_and_parse(path, &bin_data)?;
        for sym in obj_file.symbols() {
            if sym.is_undefined() {
                if let Ok(name) = sym.name() {
                    missing_symbols.insert(name.to_string());
                }
            }
        }
    }

    let mut generated = String::new();
    for sym in so_file.symbols() {
        if sym.is_definition() {
            if let Ok(name) = sym.name() {
                if missing_symbols.contains(name) {
                    _ = match sym.kind() {
                        SymbolKind::Text => writeln!(generated, "void {}() {{}}", name),
                        SymbolKind::Data => writeln!(generated, "char {}[{}];", name, sym.size()),
                        SymbolKind::Tls => writeln!(generated, "__thread char {}[{}];", name, sym.size()),
                        _ => Ok(())
                    };
                }
            }
        }
    }
    Ok(generated)
}
