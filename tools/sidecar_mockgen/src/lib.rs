// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use object::{File, Object, ObjectSymbol, Symbol, SymbolFlags, SymbolKind};
use std::collections::HashSet;
use std::fmt::Write;
use std::path::Path;
use std::{fs, io};

fn check_and_parse<'a>(
    path: &'a Path,
    bin_data: &'a io::Result<Vec<u8>>,
) -> Result<File<'a, &'a [u8]>, String> {
    let bin_data = match bin_data {
        Err(e) => {
            return Err(format!("Could not read {}: {}", path.to_string_lossy(), e));
        }
        Ok(bin_data) => bin_data,
    };
    match File::parse(bin_data.as_slice()) {
        Err(e) => Err(format!("Could not parse {}: {}", path.to_string_lossy(), e)),
        Ok(parsed) => Ok(parsed),
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

    fn sym_is_definition(sym: &Symbol) -> bool {
        if sym.is_definition() {
            return true;
        }
        match sym.flags() {
            // 10 == STT_GNU_IFUNC for ELF files
            SymbolFlags::Elf { st_info, .. } => st_info & 0xf == 10,
            _ => false,
        }
    }

    let mut generated = String::new();
    for sym in so_file.symbols().chain(so_file.dynamic_symbols()) {
        if sym_is_definition(&sym) {
            if let Ok(name) = sym.name() {
                if missing_symbols.remove(name) {
                    // strip leading underscore
                    #[cfg(target_os = "macos")]
                    let name = &name[1..];
                    _ = match sym.kind() {
                        SymbolKind::Text => {
                            if !sym.is_weak() {
                                writeln!(generated, "void {name}() {{}}")
                            } else {
                                Ok(())
                            }
                        }
                        // Ignore symbols of size 0, like _GLOBAL_OFFSET_TABLE_ on alpine
                        SymbolKind::Data | SymbolKind::Unknown => {
                            if sym.size() > 0 {
                                writeln!(generated, "char {}[{}];", name, sym.size())
                            } else {
                                #[cfg(not(target_os = "macos"))]
                                let ret = Ok(());
                                #[cfg(target_os = "macos")]
                                let ret = writeln!(generated, "char {name}[1];");
                                ret
                            }
                        }
                        SymbolKind::Tls => {
                            if sym.size() > 0 {
                                writeln!(generated, "__thread char {}[{}];", name, sym.size())
                            } else {
                                #[cfg(not(target_os = "macos"))]
                                let ret = Ok(());
                                #[cfg(target_os = "macos")]
                                let ret = writeln!(generated, "__thread char {name}[1];");
                                ret
                            }
                        }
                        _ => Ok(()),
                    };
                }
            }
        }
    }
    Ok(generated)
}
