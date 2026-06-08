// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Runtime ELF self-inspection for shared library. Verifies that the OTel thread context symbol is
//! discoverable by an out-of-process reader as required by the OTel thread-level context sharing
//! specification.
//!
//! Call [`check_tls_slot_present`] from within a cdylib context to verify that
//! this shared object was linked with the required TLS properties:
//! - `otel_thread_ctx_v1` is exported as TLS GLOBAL in the dynamic symbol table.
//! - `otel_thread_ctx_v1` is NOT accessed via General Dynamic or Local Dynamic
//!   TLS relocations (DTPMOD/DTPOFF) in `.rela.dyn`. The linker may resolve to
//!   TLSDESC or Local Exec depending on optimization; both are acceptable.
//!
//! This module is only available on Linux (the only platform that supports the
//! TLSDESC dialect used by this crate) and only when the `autocheck` feature
//! is enabled.

use elf::{abi, endian::AnyEndian, ElfBytes};
use std::path::{Path, PathBuf};

const SYMBOL: &str = "otel_thread_ctx_v1";

/// Verify TLS properties of an ELF file at the given path.
///
/// Checks that `otel_thread_ctx_v1` is exported as a TLS GLOBAL symbol with no
/// General Dynamic or Local Dynamic TLS relocations.
///
/// Returns `Ok(())` on success, or an `Err` with a diagnostic message on
/// failure (does not panic).
pub fn check_tls_slot_in(path: &Path) -> Result<(), String> {
    let data =
        std::fs::read(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let elf = ElfBytes::<AnyEndian>::minimal_parse(&data)
        .map_err(|e| format!("failed to parse ELF at {}: {e}", path.display()))?;
    check_dynsym(&elf)?;
    check_no_gd_ld_reloc(&elf)?;
    Ok(())
}

/// Same as [`check_tls_slot_in`], but automatically locates this shared object
/// via `/proc/self/maps`. Intended for use from within the cdylib at runtime.
pub fn check_tls_slot_present() -> Result<(), String> {
    check_tls_slot_in(&own_so_path()?)
}

/// Locate this shared object via `/proc/self/maps` using `check_linking`'s address.
fn own_so_path() -> Result<PathBuf, String> {
    let addr = check_tls_slot_present as *const () as usize;
    let maps = std::fs::read_to_string("/proc/self/maps")
        .map_err(|e| format!("failed to read /proc/self/maps: {e}"))?;
    for line in maps.lines() {
        // Format: address perms offset dev inode [pathname]
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 6 {
            continue;
        }
        let path = fields[5];
        if !path.starts_with('/') {
            continue;
        }
        if let Some((start_str, end_str)) = fields[0].split_once('-') {
            let start = usize::from_str_radix(start_str, 16).unwrap_or(0);
            let end = usize::from_str_radix(end_str, 16).unwrap_or(0);
            if addr >= start && addr < end {
                return Ok(PathBuf::from(path));
            }
        }
    }
    Err("could not find our shared object in /proc/self/maps".into())
}

fn check_dynsym(elf: &ElfBytes<'_, AnyEndian>) -> Result<(), String> {
    let (symtab, strtab) = elf
        .dynamic_symbol_table()
        .map_err(|e| format!("failed to read .dynsym: {e}"))?
        .ok_or_else(|| "no dynamic symbol table found".to_string())?;
    let found = symtab.iter().any(|sym| {
        strtab
            .get(sym.st_name as usize)
            .map(|name| {
                name == SYMBOL
                    && sym.st_symtype() == abi::STT_TLS
                    && sym.st_bind() == abi::STB_GLOBAL
            })
            .unwrap_or(false)
    });
    if !found {
        return Err(format!(
            "'{SYMBOL}' not found as TLS GLOBAL in dynamic symbol table"
        ));
    }
    Ok(())
}

fn check_no_gd_ld_reloc(elf: &ElfBytes<'_, AnyEndian>) -> Result<(), String> {
    #[cfg(target_arch = "x86_64")]
    const FORBIDDEN_RELOCS: &[(u32, &str)] = &[
        (16, "R_X86_64_DTPMOD64"),
        (17, "R_X86_64_DTPOFF64"),
    ];
    #[cfg(target_arch = "aarch64")]
    const FORBIDDEN_RELOCS: &[(u32, &str)] = &[
        (1028, "R_AARCH64_TLS_DTPMOD"),
        (1029, "R_AARCH64_TLS_DTPREL"),
    ];

    let (symtab, strtab) = elf
        .dynamic_symbol_table()
        .map_err(|e| format!("failed to read .dynsym: {e}"))?
        .ok_or_else(|| "no dynamic symbol table found".to_string())?;
    let sym_idx = symtab
        .iter()
        .enumerate()
        .find(|(_, sym)| {
            strtab
                .get(sym.st_name as usize)
                .map(|n| n == SYMBOL)
                .unwrap_or(false)
        })
        .map(|(i, _)| i as u32)
        .ok_or_else(|| format!("'{SYMBOL}' not found in .dynsym"))?;

    let rela_shdr = elf
        .section_header_by_name(".rela.dyn")
        .map_err(|e| format!("failed to read section headers: {e}"))?;

    if let Some(rela_shdr) = rela_shdr {
        let bad: Vec<&str> = elf
            .section_data_as_relas(&rela_shdr)
            .map_err(|e| format!("failed to read .rela.dyn: {e}"))?
            .filter(|r| r.r_sym == sym_idx)
            .filter_map(|r| {
                FORBIDDEN_RELOCS
                    .iter()
                    .find(|(typ, _)| *typ == r.r_type)
                    .map(|(_, name)| *name)
            })
            .collect();
        if !bad.is_empty() {
            return Err(format!(
                "'{SYMBOL}' has General Dynamic / Local Dynamic relocations in .rela.dyn: {}. \
                 Expected TLSDESC or Local Exec instead.",
                bad.join(", ")
            ));
        }
    }

    Ok(())
}
