// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use object::{File, Object, ObjectSection, ObjectSymbol, Symbol, SymbolFlags, SymbolKind};
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

/// Weaken symbols present in a binary in relocatable objects (`.o`) in place.
pub fn weaken_object_symbols(target: &Path, binary: &Path) -> Result<(), String> {
    let data = fs::read(target)
        .map_err(|e| format!("read {}: {e}", target.display()))?;

    let undefined_candidates: HashSet<String> = File::parse(data.as_slice())
        .map_err(|e| format!("parse {}: {e}", target.display()))?.symbols()
            .filter(|s| s.is_undefined() && !s.is_weak())
            .filter_map(|s| s.name().ok().map(|n| n.to_string()))
            .collect();

    // Filter symbols from binary.
    let symbols = {
        let bin_data = fs::read(binary)
            .map_err(|e| format!("read {}: {e}", binary.display()))?;
        let so_file = File::parse(bin_data.as_slice())
            .map_err(|e| format!("parse {}: {e}", binary.display()))?;
        let mut result = HashSet::new();
        for sym in so_file.symbols().chain(so_file.dynamic_symbols()) {
            if sym_is_definition(&sym) {
                if let Ok(name) = sym.name() {
                    if undefined_candidates.contains(name) {
                        #[cfg(target_os = "macos")]
                        let name = &name[1..];
                        result.insert(name.to_string());
                    }
                }
            }
        }
        result
    };

    weaken_symtab(target, &symbols)
}

/// Weaken select symbols in the `.symtab` of an ELF relocatable object (`.o`).
/// 
/// Locates each symbol by index in the `.symtab` section, then flips `st_bind` from `STB_GLOBAL(1)`
/// to `STB_WEAK(2)` for the given `symbols` set.
fn weaken_symtab(obj_path: &Path, symbols: &HashSet<String>) -> Result<(), String> {
    let mut data = fs::read(obj_path)
        .map_err(|e| format!("read {}: {e}", obj_path.display()))?;

    let patches: Vec<usize> = {
        let elf = File::parse(data.as_slice())
            .map_err(|e| format!("parse {}: {e}", obj_path.display()))?;

        let symtab = match elf.section_by_name(".symtab") {
            Some(s) => s,
            None => return Ok(()), // no .symtab — nothing to do
        };
        let (symtab_off, _) = symtab
            .file_range()
            .ok_or_else(|| format!("{}: .symtab has no file range", obj_path.display()))?;
        let entsize: u64 = 24; // sizeof(Elf64_Sym); st_info at byte offset 4

        elf.symbols()
            .filter(|sym| {
                sym.is_undefined()
                    && !sym.is_weak()
                    && sym.name().map_or(false, |n| symbols.contains(n))
            })
            .map(|sym| (symtab_off + sym.index().0 as u64 * entsize + 4) as usize)
            .collect()
    };

    if patches.is_empty() {
        return Ok(());
    }
    for st_info_pos in patches {
        let old = data[st_info_pos];
        data[st_info_pos] = (2u8 << 4) | (old & 0xf); // STB_WEAK = 2
    }
    fs::write(obj_path, &data)
        .map_err(|e| format!("write {}: {e}", obj_path.display()))
}