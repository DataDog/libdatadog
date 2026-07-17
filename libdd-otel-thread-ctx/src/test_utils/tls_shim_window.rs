// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared logic for locating and hashing the TLSDESC access sequence of `otel_thread_ctx_v1` in an
//! ELF object.
//!
//! This module has two consumers in `libdd-otel-thread-ctx-ffi`:
//! - the `tlsdesc_inline_sequence` integration test, which extracts the sequence from our own
//!   statically linked artifact (produced by the inline assembly in `libdd_otel_thread_ctx`) and
//!   hashes it
//! - the `gen_tls_shim_hash` binary, which extracts the sequence from a Clang-compiled reference
//!   object and hashes it to produce the "golden" hash
//!
//! Both consumers must agree byte-for-byte, so the extraction lives here once and is shared.
//!
//! The target architecture is read from the ELF header at runtime, so the generator can process a
//! cross-compiled reference object for either architecture from any host.
//!
//! The "window" is the full TLSDESC access sequence, in program order:
//! - **x86-64**: `lea …@tlsdesc(%rip), %rax` / `call *…@tlscall(%rax)` / `add %fs:0x0, %rax`.
//! - **aarch64**: `adrp`/`ldr`/`add`/`blr` (the relocation-bearing core) followed by the
//!   thread-pointer computation `mrs x8, tpidr_el0` / `add x0, x8, x0`.
//!
//! The window is located from the object's TLSDESC relocations and then sliced out with a fixed,
//! architecture-specific offset formula that matches how both Clang and our inline assembly lay the
//! sequence out (contiguously, thread-pointer read immediately after the call). The
//! relocation-bearing immediates are zero in the object file (the linker fills them in), so the
//! sliced bytes are stable.
//!
//! This window location and size might have to be patched if future versions of clang generate a
//! different access sequence in the future.

use std::{path::Path, str::FromStr};

use elf::{abi, endian::AnyEndian, symbol::SymbolTable, ElfBytes};
use object::read::archive::ArchiveFile;
use sha2::{Digest, Sha256};

/// The exported TLS symbol whose access sequence we hash.
pub const SYMBOL: &str = "otel_thread_ctx_v1";

/// The architectures for which we know how to locate and slice a TLSDESC sequence.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arch {
    X86_64,
    Aarch64,
}

impl FromStr for Arch {
    type Err = String;

    fn from_str(name: &str) -> Result<Arch, String> {
        match name {
            "x86_64" | "amd64" | "x86-64" => Ok(Arch::X86_64),
            "aarch64" | "arm64" => Ok(Arch::Aarch64),
            other => Err(format!(
                "unknown architecture `{other}` (expected x86_64 or aarch64)"
            )),
        }
    }
}

impl Arch {
    /// The architecture of the host this code was compiled for.
    pub fn host() -> Arch {
        if cfg!(target_arch = "aarch64") {
            Arch::Aarch64
        } else {
            Arch::X86_64
        }
    }

    /// The `--target=` triple to hand a cross-compiling Clang for this architecture.
    pub fn clang_target_triple(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64-unknown-linux-gnu",
            Arch::Aarch64 => "aarch64-unknown-linux-gnu",
        }
    }

    fn from_e_machine(e_machine: u16, label: &str) -> Arch {
        match e_machine {
            abi::EM_X86_64 => Arch::X86_64,
            abi::EM_AARCH64 => Arch::Aarch64,
            other => panic!("{label} has unsupported ELF machine type {other}"),
        }
    }

    /// Number of object-file TLSDESC relocations a single access emits.
    fn relocations_per_access(self) -> usize {
        match self {
            // `lea` (GOTPC32_TLSDESC) + `call` (TLSDESC_CALL).
            Arch::X86_64 => 2,
            // `adrp` (ADR_PAGE21) + `ldr` (LD64_LO12) + `add` (ADD_LO12) + `blr` (CALL).
            Arch::Aarch64 => 4,
        }
    }

    fn is_tlsdesc_object_relocation(self, relocation_type: u32) -> bool {
        // `R_*_TLSDESC` (x86-64) / `R_AARCH64_TLSDESC` are the dynamic-linker relocations emitted
        // after linking, so they are intentionally excluded here.
        match self {
            Arch::X86_64 => matches!(
                relocation_type,
                abi::R_X86_64_GOTPC32_TLSDESC | abi::R_X86_64_TLSDESC_CALL
            ),
            Arch::Aarch64 => matches!(
                relocation_type,
                abi::R_AARCH64_TLSDESC_LD_PREL19
                    | abi::R_AARCH64_TLSDESC_ADR_PREL21
                    | abi::R_AARCH64_TLSDESC_ADR_PAGE21
                    | abi::R_AARCH64_TLSDESC_LD64_LO12
                    | abi::R_AARCH64_TLSDESC_ADD_LO12
                    | abi::R_AARCH64_TLSDESC_OFF_G1
                    | abi::R_AARCH64_TLSDESC_OFF_G0_NC
                    | abi::R_AARCH64_TLSDESC_LDR
                    | abi::R_AARCH64_TLSDESC_ADD
                    | abi::R_AARCH64_TLSDESC_CALL
            ),
        }
    }

    /// Compute the `[start, end)` byte bounds of the full TLSDESC sequence within its section, from
    /// the (offset-sorted) relocation offsets of a single access.
    fn sequence_bounds(self, offsets: &[u64], section_len: usize) -> (usize, usize) {
        let (start, end) = match self {
            // x86-64: the first relocation sits on the `lea` displacement (3 bytes into the
            // instruction) and the second on the `call`. The trailing `add %fs:0x0, %rax` ends 11
            // bytes past the `call` relocation.
            Arch::X86_64 => {
                let first = to_usize(offsets[0], "first relocation offset");
                let call = to_usize(offsets[1], "call relocation offset");
                let start = first
                    .checked_sub(3)
                    .expect("x86-64 relocation offset is before the LEA displacement");
                (start, call + 11)
            }
            // aarch64: the four relocations sit on `adrp`/`ldr`/`add`/`blr`. The window starts at
            // the `adrp` and extends 12 bytes past the last relocation: `blr` (4) followed by the
            // two non-relocated instructions `mrs x8, tpidr_el0` (4) and `add x0, x8, x0` (4).
            Arch::Aarch64 => {
                let start = to_usize(offsets[0], "first relocation offset");
                let last = to_usize(offsets[offsets.len() - 1], "last relocation offset");
                (start, last + 12)
            }
        };
        assert!(
            end <= section_len,
            "{self:?} TLSDESC sequence extends beyond section data"
        );
        (start, end)
    }
}

fn to_usize(value: u64, what: &str) -> usize {
    usize::try_from(value).unwrap_or_else(|_| panic!("{what} does not fit in usize"))
}

/// A single TLSDESC access sequence extracted from an object, ready to be hashed.
pub struct TlsDescWindow {
    /// Human-readable origin (file, archive member, …), used in messages.
    pub label: String,
    /// The detected architecture of the object the sequence came from.
    pub arch: Arch,
    /// The full sequence bytes, sliced out with the architecture-specific bounds.
    pub bytes: Vec<u8>,
}

impl TlsDescWindow {
    /// Lowercase hex SHA-256 of the sequence bytes.
    pub fn hash_hex(&self) -> String {
        let digest = Sha256::digest(&self.bytes);
        let mut out = String::with_capacity(digest.len() * 2);
        for byte in digest {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }

    pub fn hex_dump(&self) -> String {
        match self.arch {
            Arch::Aarch64 => self
                .bytes
                .chunks(4)
                .map(|word| word.iter().map(|b| format!("{b:02x}")).collect::<String>())
                .collect::<Vec<_>>()
                .join(" "),
            Arch::X86_64 => self.bytes.iter().map(|b| format!("{b:02x}")).collect(),
        }
    }
}

fn parse_elf<'data>(data: &'data [u8], label: &str) -> ElfBytes<'data, AnyEndian> {
    ElfBytes::<AnyEndian>::minimal_parse(data)
        .unwrap_or_else(|e| panic!("failed to parse ELF data from {label}: {e}"))
}

fn symbol_indexes_in_table(
    elf: &ElfBytes<'_, AnyEndian>,
    symtab_index: usize,
    label: &str,
) -> Vec<u32> {
    let Some(section_headers) = elf.section_headers() else {
        panic!("{label} has no ELF section headers");
    };
    let symtab_header = section_headers
        .get(symtab_index)
        .unwrap_or_else(|e| panic!("failed to read symbol table header {symtab_index}: {e}"));

    // Relocation sections link to the symbol table they use; archive members usually use
    // `.symtab`, while linked dynamic artifacts may use `.dynsym`.
    if !matches!(symtab_header.sh_type, abi::SHT_SYMTAB | abi::SHT_DYNSYM) {
        return Vec::new();
    }

    let strtab_header = section_headers
        .get(symtab_header.sh_link as usize)
        .unwrap_or_else(|e| panic!("failed to read linked string table header: {e}"));
    let strtab = elf
        .section_data_as_strtab(&strtab_header)
        .unwrap_or_else(|e| panic!("failed to read linked string table in {label}: {e}"));
    let (symtab_data, _) = elf
        .section_data(&symtab_header)
        .unwrap_or_else(|e| panic!("failed to read symbol table data in {label}: {e}"));
    let symtab = SymbolTable::new(elf.ehdr.endianness, elf.ehdr.class, symtab_data);

    symtab
        .iter()
        .enumerate()
        .filter_map(|(index, sym)| {
            strtab
                .get(sym.st_name as usize)
                .ok()
                .filter(|name| *name == SYMBOL)
                .map(|_| index as u32)
        })
        .collect()
}

/// Extract every TLSDESC access sequence for [`SYMBOL`] from a single ELF object blob.
pub fn windows_in_elf(data: &[u8], label: &str) -> Vec<TlsDescWindow> {
    let elf = parse_elf(data, label);
    let arch = Arch::from_e_machine(elf.ehdr.e_machine, label);
    let Some(section_headers) = elf.section_headers() else {
        panic!("{label} has no ELF section headers");
    };
    let mut windows = Vec::new();

    for section_header in section_headers
        .iter()
        .filter(|shdr| matches!(shdr.sh_type, abi::SHT_REL | abi::SHT_RELA))
    {
        let symbol_indexes = symbol_indexes_in_table(&elf, section_header.sh_link as usize, label);
        if symbol_indexes.is_empty() {
            continue;
        }

        let target_header = section_headers
            .get(section_header.sh_info as usize)
            .unwrap_or_else(|e| panic!("failed to read relocation target section header: {e}"));
        let (target_data, _) = elf
            .section_data(&target_header)
            .unwrap_or_else(|e| panic!("failed to read relocation target section in {label}: {e}"));

        let mut offsets = Vec::new();
        match section_header.sh_type {
            abi::SHT_REL => {
                let rels = elf
                    .section_data_as_rels(&section_header)
                    .unwrap_or_else(|e| panic!("failed to read REL relocations in {label}: {e}"));
                offsets.extend(
                    rels.filter(|rel| {
                        symbol_indexes.contains(&rel.r_sym)
                            && arch.is_tlsdesc_object_relocation(rel.r_type)
                    })
                    .map(|rel| rel.r_offset),
                );
            }
            abi::SHT_RELA => {
                let relas = elf
                    .section_data_as_relas(&section_header)
                    .unwrap_or_else(|e| panic!("failed to read RELA relocations in {label}: {e}"));
                offsets.extend(
                    relas
                        .filter(|rela| {
                            symbol_indexes.contains(&rela.r_sym)
                                && arch.is_tlsdesc_object_relocation(rela.r_type)
                        })
                        .map(|rela| rela.r_offset),
                );
            }
            _ => unreachable!(),
        }

        if offsets.is_empty() {
            continue;
        }

        offsets.sort_unstable();
        let per_access = arch.relocations_per_access();
        assert!(
            offsets.len() % per_access == 0,
            "expected TLSDESC relocations for {SYMBOL} in {label} to come in groups of \
             {per_access}; found {} offsets",
            offsets.len()
        );

        for chunk in offsets.chunks_exact(per_access) {
            let (start, end) = arch.sequence_bounds(chunk, target_data.len());
            windows.push(TlsDescWindow {
                label: label.to_owned(),
                arch,
                bytes: target_data[start..end].to_vec(),
            });
        }
    }

    windows
}

/// Extract every TLSDESC access sequence for [`SYMBOL`] from a single ELF object file on disk.
pub fn windows_in_object_file(path: &Path) -> Vec<TlsDescWindow> {
    let data =
        std::fs::read(path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    windows_in_elf(&data, &path.display().to_string())
}

/// Extract every TLSDESC access sequence for [`SYMBOL`] from every ELF member of a static archive.
pub fn windows_in_archive(path: &Path) -> Vec<TlsDescWindow> {
    let archive_data =
        std::fs::read(path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let archive = ArchiveFile::parse(&*archive_data)
        .unwrap_or_else(|e| panic!("failed to parse archive {}: {e}", path.display()));

    let mut windows = Vec::new();
    for member in archive.members() {
        let member =
            member.unwrap_or_else(|e| panic!("failed to read member in {}: {e}", path.display()));
        let member_data = member.data(&*archive_data).unwrap_or_else(|e| {
            panic!(
                "failed to read member data for {} in {}: {e}",
                String::from_utf8_lossy(member.name()),
                path.display()
            )
        });

        if member_data.starts_with(&abi::ELFMAGIC) {
            let member_name = String::from_utf8_lossy(member.name()).into_owned();
            let label = format!("{}({member_name})", path.display());
            windows.extend(windows_in_elf(member_data, &label));
        }
    }

    windows
}
