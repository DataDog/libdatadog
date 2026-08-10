// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! ELF GOT-patching primitives for runtime function interposition.
//!
//! Walks each loaded ELF object via `dl_iterate_phdr`, parses its
//! `PT_DYNAMIC` for the symbol/string/hash tables and the relocation
//! arrays, and provides utilities to rewrite GOT entries.
//!
//! Scope:
//! * 64-bit Linux ELF only (`Elf64_*`). Other targets are compile-time gated.
//! * `DT_GNU_HASH` for determining dynsym entry count, with a symtab/strtab distance heuristic
//!   fallback.
//! * REL / RELA / JMPREL relocation arrays.

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
mod elf;

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub use elf::*;
