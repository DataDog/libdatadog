// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Runtime ELF self-inspection for the ddheap USDT probes. Verifies that each
//! probe has exactly one `.note.stapsdt` entry, so an attached consumer
//! (bpftrace / eBPF) sees one probe per site rather than attaching twice.
//!
//! The probes are emitted as non-inline functions in a dedicated translation
//! unit (`probes.c`) precisely so each `USDT()` expansion produces a single
//! note; a regression (e.g. `static inline` + bindgen's `wrap_static_fns`, or
//! LTO inlining across TUs) could duplicate the entry. This check catches that.
//!
//! Call [`sanity_check`] from within a shared object or statically linked
//! executable, or point [`check_usdt_probes_in`] at a built artifact.
//!
//! Only available on Linux (USDT/SystemTap notes are Linux-only) and only when
//! the `sanity-check` feature is enabled.

use anyhow::{bail, Context};
use elf::{endian::AnyEndian, ElfBytes};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// USDT provider name emitted by `probes.c` (`USDT(ddheap, ...)`).
const PROVIDER: &str = "ddheap";
/// Probes we expect, each exactly once.
const EXPECTED_PROBES: &[&str] = &["alloc", "free"];

/// As [`sanity_check`], but takes the object file as an argument. Useful for a
/// test setting where the test code is separate from the artifact to validate.
pub fn check_usdt_probes_in(path: &Path) -> anyhow::Result<()> {
    let data = std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let elf = ElfBytes::<AnyEndian>::minimal_parse(&data)
        .with_context(|| format!("failed to parse ELF at {}", path.display()))?;
    check_one_note_per_probe(&elf)?;
    Ok(())
}

/// Check that the current running module carries exactly one `.note.stapsdt`
/// entry per ddheap probe.
pub fn sanity_check() -> anyhow::Result<()> {
    check_usdt_probes_in(&own_elf_path()?)
}

/// Locate the current running module (shared or not) via `/proc/self/maps`.
fn own_elf_path() -> anyhow::Result<PathBuf> {
    // We use the address of an arbitrary function of this module.
    let addr = sanity_check as *const () as usize;
    let maps =
        std::fs::read_to_string("/proc/self/maps").context("failed to read /proc/self/maps")?;
    for line in maps.lines() {
        // Format: address perms offset dev inode [pathname]
        // Skip the first 5 whitespace-delimited tokens then take the rest
        // verbatim as the path, so pathnames containing spaces are preserved.
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

/// Parse `.note.stapsdt` and assert each expected probe appears exactly once.
fn check_one_note_per_probe(elf: &ElfBytes<'_, AnyEndian>) -> anyhow::Result<()> {
    let shdr = elf
        .section_header_by_name(".note.stapsdt")
        .context("failed to read section headers")?
        .context("no .note.stapsdt section: USDT probes are missing from this object")?;

    let notes = elf
        .section_data_as_notes(&shdr)
        .context("failed to parse .note.stapsdt")?;

    // Pointer width for the stapsdt descriptor's three leading addresses.
    let word = if elf.ehdr.class == elf::file::Class::ELF64 {
        8
    } else {
        4
    };

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for note in notes {
        // stapsdt notes are emitted with name "stapsdt"; skip anything else.
        let elf::note::Note::Unknown(any) = note else {
            continue;
        };
        if any.name != "stapsdt" {
            continue;
        }
        // Descriptor: 3 target-word addresses (location, base, semaphore),
        // then NUL-terminated provider, probe, and arg-format strings.
        let strings = any.desc.get(3 * word..).unwrap_or(&[]);
        let mut parts = strings.split(|&b| b == 0);
        let provider = parts.next().unwrap_or(&[]);
        let probe = parts.next().unwrap_or(&[]);
        if provider == PROVIDER.as_bytes() {
            if let Ok(name) = std::str::from_utf8(probe) {
                *counts.entry(name.to_string()).or_default() += 1;
            }
        }
    }

    for &probe in EXPECTED_PROBES {
        match counts.get(probe).copied().unwrap_or(0) {
            1 => {}
            0 => bail!("USDT probe '{PROVIDER}:{probe}' has no .note.stapsdt entry"),
            n => bail!(
                "USDT probe '{PROVIDER}:{probe}' has {n} .note.stapsdt entries, expected 1 \
                 (duplicate entries make an attached consumer fire twice)"
            ),
        }
    }
    Ok(())
}
