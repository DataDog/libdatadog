// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Runtime ELF self-inspection for shared library. Verifies that the OTel thread context symbol is
//! discoverable by an out-of-process reader as required by the OTel thread-level context sharing
//! specification.
//!
//! Call [`sanity_check`] from within a shared object or a statically linked executables to verify
//! that the binary was linked with the correct option:
//!
//! - `otel_thread_ctx_v1` is exported as TLS GLOBAL in the dynamic symbol table.
//! - `otel_thread_ctx_v1` has no non-TLSDESC TLS relocations in `.rela.dyn`. The linker may pick
//!   TLSDESC or Local Exec depending on optimization; both are acceptable. All other TLS relocation
//!   types (DTPMOD, DTPOFF, TPOFF, GOTTPOFF, etc.) are rejected.
//!
//! This module is only available on Linux (the only platform that supports the TLSDESC dialect used
//! by this crate) and only when the `sanity-check` feature is enabled.

use anyhow::{bail, Context};
use elf::{abi, endian::AnyEndian, ElfBytes};
use std::path::{Path, PathBuf};

const SYMBOL: &str = "otel_thread_ctx_v1";

/// Safe as [sanity_check], but takes the object file as an argument. Useful for a test setting
/// where the test code is separate from the artifact to validate.
pub fn check_tls_slot_in(path: &Path) -> anyhow::Result<()> {
    let data = std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let elf = ElfBytes::<AnyEndian>::minimal_parse(&data)
        .with_context(|| format!("failed to parse ELF at {}", path.display()))?;
    check_dynsym(&elf)?;
    check_tlsdesc_reloc_only(&elf)?;
    Ok(())
}

/// Check that the current running module has been linked appropriately to make the OTel shared
/// thread context discoverable.
///
/// Checks that `otel_thread_ctx_v1` is exported as a TLS GLOBAL symbol and that any TLS
/// relocations targeting it are TLSDESC. No relocation (Local Exec/static binary) is also
/// acceptable.
pub fn sanity_check() -> anyhow::Result<()> {
    check_tls_slot_in(&own_elf_path()?)
}

/// Locate the current running module (shared or not) via `/proc/self/maps`.
fn own_elf_path() -> anyhow::Result<PathBuf> {
    // We use the address of an arbitrary function of this module.
    let addr = sanity_check as *const () as usize;
    let maps =
        std::fs::read_to_string("/proc/self/maps").context("failed to read /proc/self/maps")?;
    for line in maps.lines() {
        // Format: address perms offset dev inode [pathname]
        // Skip the first 5 whitespace-delimited tokens then take the rest verbatim
        // as the path, so that pathnames containing spaces are preserved intact.
        let mut rest = line;

        for _ in 0..5 {
            rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
            rest = rest.trim_start_matches(|c: char| !c.is_ascii_whitespace());
        }

        let path = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());

        if !path.starts_with('/') {
            continue;
        }

        if let Some((start_str, end_str)) = line
            .split_whitespace()
            .next()
            .and_then(|f| f.split_once('-'))
        {
            let start = usize::from_str_radix(start_str, 16).unwrap_or(0);
            let end = usize::from_str_radix(end_str, 16).unwrap_or(0);
            if addr >= start && addr < end {
                return Ok(PathBuf::from(path));
            }
        }
    }
    bail!("could not find our own object file in /proc/self/maps")
}

/// Check that [SYMBOL] is present in the `.dynsym` table of the ELF data.
fn check_dynsym(elf: &ElfBytes<'_, AnyEndian>) -> anyhow::Result<()> {
    let (symtab, strtab) = elf
        .dynamic_symbol_table()
        .context("failed to read .dynsym")?
        .context("no dynamic symbol table found")?;
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
        bail!("'{SYMBOL}' not found as TLS GLOBAL in dynamic symbol table");
    }
    Ok(())
}

/// Check that any relocation for [SYMBOL] in `.rela.dyn` is a TLSDESC relocation. No relocation at
/// all (Local Exec / static binary) is also acceptable. All other TLS relocation types (DTPMOD,
/// DTPOFF, TPOFF, GOTTPOFF, etc.) are rejected.
fn check_tlsdesc_reloc_only(elf: &ElfBytes<'_, AnyEndian>) -> anyhow::Result<()> {
    #[cfg(target_arch = "x86_64")]
    const TLSDESC_RELOC: u32 = 36; // R_X86_64_TLSDESC
    #[cfg(target_arch = "aarch64")]
    const TLSDESC_RELOC: u32 = 1031; // R_AARCH64_TLSDESC

    let (symtab, strtab) = elf
        .dynamic_symbol_table()
        .context("failed to read .dynsym")?
        .context("no dynamic symbol table found")?;
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
        .with_context(|| format!("'{SYMBOL}' not found in .dynsym"))?;

    let rela_shdr = elf
        .section_header_by_name(".rela.dyn")
        .context("failed to read section headers")?;

    if let Some(rela_shdr) = rela_shdr {
        let bad: Vec<u32> = elf
            .section_data_as_relas(&rela_shdr)
            .context("failed to read .rela.dyn")?
            .filter(|r| r.r_sym == sym_idx && r.r_type != TLSDESC_RELOC)
            .map(|r| r.r_type)
            .collect();
        if !bad.is_empty() {
            let types: Vec<String> = bad.iter().map(|t| format!("type {t}")).collect();
            bail!(
                "'{SYMBOL}' has non-TLSDESC relocations in .rela.dyn: {}. \
                 Only TLSDESC or no relocation (Local Exec) is accepted.",
                types.join(", ")
            );
        }
    }

    Ok(())
}
