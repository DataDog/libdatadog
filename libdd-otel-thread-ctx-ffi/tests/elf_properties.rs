// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Verify ELF properties of the shared library built on Linux. Running the sanity check in
//! [libdd_otel_thread_ctx] directly in a Rust test would exercise the static linking case. This
//! test rather checks that the dynamic library is properly linked, which is why it lives within the
//! FFI.
//!
//! These tests check that:
//! - `otel_thread_ctx_v1` is exported in the dynamic symbol table as a TLS GLOBAL symbol.
//! - `otel_thread_ctx_v1` follows the TLSDESC access model: if there is a relocation for it, it is
//!   a TLSDESC relocation.
//! - The Rust inline-asm TLSDESC access matches what a C compiler generates. The comparison has two
//!   parts, because gcc and clang (and different gcc versions) legitimately differ on scratch
//!   registers and on how they schedule the thread-pointer read.
//!   1. The relocation-bearing core the linker relaxes is compared byte-for-byte, up to the
//!      descriptor scratch register: `adrp`/`ldr`/`add`/`blr` on aarch64, `lea`/`call` plus the
//!      `%fs:0` add on x86-64.
//!   2. On aarch64, check the thread-pointer computation (`mrs tpidr_el0` and `add`) is located in
//!      a small window around the core rather than pinned to a fixed position, since compilers
//!      schedule it freely (gcc may hoist the `mrs` above the sequence and/or interleave the
//!      function epilogue before the final `add`). Together this guarantees linker TLS relaxation
//!      works identically to a compiler-generated access, while tolerating the parts that are
//!      genuinely compiler-defined.
//!
//! Library artifact paths are derived at runtime from the test executable location.
//! The test binary and crate artifacts live in `target/<[triple/]profile>/deps/`.

#![cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use std::{
    fmt,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use elf::{abi, endian::AnyEndian, symbol::SymbolTable, ElfBytes};
use object::read::archive::ArchiveFile;

const SYMBOL: &str = "otel_thread_ctx_v1";
const SKIP_TLS_SHIM_ASM_TEST_ENV: &str = "LIBDD_OTEL_THREAD_CTX_SKIP_TLS_SHIM_ASM_TEST";

#[derive(Clone, Copy, PartialEq, Eq)]
struct RelocationType(u32);

impl fmt::Debug for RelocationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct TlsDescSequence {
    /// The relocation-bearing instructions the linker relaxes, compared byte-for-byte (up to one
    /// register) between our inline asm and the C compiler output.
    ///
    /// - x86-64: `lea`/`call` plus the trailing `add %fs:0x0, %rax` (identical across gcc/clang).
    /// - aarch64: `adrp`/`ldr`/`add`/`blr`, with the descriptor scratch register masked out. gcc
    ///   parks the thread pointer in `x1` and uses `x2` for the descriptor, whereas clang and our
    ///   asm use `x1`.
    core_instructions: Vec<u8>,
    relocations: Vec<Relocation>,
}

/// A single TLSDESC access extracted from an object file: the comparable [`TlsDescSequence`] plus,
/// on aarch64, the surrounding instruction window used to locate the thread-pointer computation.
#[derive(Debug)]
struct TlsDescAccess {
    sequence: TlsDescSequence,
    #[cfg(target_arch = "aarch64")]
    tp_window: TpWindow,
}

fn deps_dir() -> PathBuf {
    // test binary: target/<[triple/]profile>/deps/<name>
    let exe = std::env::current_exe().expect("failed to read current executable path");
    exe.parent()
        .expect("unexpected test executable path structure")
        .to_owned()
}

fn artifact_path(name: &str) -> PathBuf {
    deps_dir().join(name)
}

fn cdylib_path() -> PathBuf {
    artifact_path("liblibdd_otel_thread_ctx_ffi.so")
}

fn staticlib_path() -> PathBuf {
    artifact_path("liblibdd_otel_thread_ctx_ffi.a")
}

fn check_readable(path: &Path) {
    assert!(
        std::fs::File::open(path).is_ok(),
        "{} could not be opened for reading",
        path.display()
    );
}

fn tool_available(tool: &str) -> bool {
    match Command::new(tool)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(_) => true,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            eprintln!("skipping test: required tool `{tool}` is not available");
            false
        }
        Err(e) => panic!("failed to check whether `{tool}` is available: {e}"),
    }
}

fn required_tools_available(tools: &[&str]) -> bool {
    tools.iter().all(|tool| tool_available(tool))
}

fn native_target() -> bool {
    let cross_compiling = option_env!("LIBDD_OTEL_THREAD_CTX_FFI_CROSS_COMPILING") == Some("true");
    if cross_compiling {
        eprintln!("skipping test: cross-compiling");
    }
    !cross_compiling
}

fn skip_tls_shim_asm_test() -> bool {
    let skip = std::env::var_os(SKIP_TLS_SHIM_ASM_TEST_ENV).is_some();
    if skip {
        eprintln!("skipping test: {SKIP_TLS_SHIM_ASM_TEST_ENV} is set");
    }
    skip
}

fn assert_command_success(command: &mut Command) {
    let out = command
        .output()
        .unwrap_or_else(|e| panic!("failed to run {command:?}: {e}"));
    assert!(
        out.status.success(),
        "{command:?} failed with status {}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn build_dir(name: &str) -> PathBuf {
    let dir = deps_dir().join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)
        .unwrap_or_else(|e| panic!("failed to create {}: {e}", dir.display()));
    dir
}

fn parse_elf<'data>(data: &'data [u8], label: &str) -> ElfBytes<'data, AnyEndian> {
    ElfBytes::<AnyEndian>::minimal_parse(data)
        .unwrap_or_else(|e| panic!("failed to parse ELF data from {label}: {e}"))
}

fn symbol_indexes_in_table(
    elf: &ElfBytes<'_, AnyEndian>,
    symtab_index: usize,
    symbol: &str,
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
                .filter(|name| *name == symbol)
                .map(|_| index as u32)
        })
        .collect()
}

fn for_each_archive_elf_member(path: &Path, mut f: impl FnMut(&str, &[u8])) {
    let archive_data =
        std::fs::read(path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let archive = ArchiveFile::parse(&*archive_data)
        .unwrap_or_else(|e| panic!("failed to parse archive {}: {e}", path.display()));

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
            let member_name = std::str::from_utf8(member.name()).unwrap_or_else(|e| {
                panic!(
                    "archive member name in {} is not valid UTF-8: {e}",
                    path.display()
                )
            });
            f(member_name, member_data);
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn is_tlsdesc_object_relocation(relocation_type: RelocationType) -> bool {
    // These are object-file TLSDESC relocations. `R_X86_64_TLSDESC` is the dynamic-linker
    // relocation emitted after linking, so it is intentionally excluded here.
    matches!(
        relocation_type.0,
        abi::R_X86_64_GOTPC32_TLSDESC | abi::R_X86_64_TLSDESC_CALL
    )
}

#[cfg(target_arch = "aarch64")]
fn is_tlsdesc_object_relocation(relocation_type: RelocationType) -> bool {
    // These are object-file TLSDESC relocations. `R_AARCH64_TLSDESC` is the dynamic-linker
    // relocation emitted after linking, so it is intentionally excluded here.
    matches!(
        relocation_type.0,
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
    )
}

#[derive(Debug, PartialEq, Eq)]
struct Relocation {
    offset: u64,
    relocation_type: RelocationType,
    addend: i64,
}

#[cfg(target_arch = "x86_64")]
const TLSDESC_RELOCATIONS_PER_ACCESS: usize = 2;

#[cfg(target_arch = "aarch64")]
const TLSDESC_RELOCATIONS_PER_ACCESS: usize = 4;

#[cfg(target_arch = "x86_64")]
fn tlsdesc_sequence_bounds(relocations: &[Relocation], section_len: usize) -> (usize, usize) {
    let first_offset = usize::try_from(relocations[0].offset)
        .expect("first relocation offset does not fit in usize");
    let call_offset = usize::try_from(relocations[1].offset)
        .expect("call relocation offset does not fit in usize");
    let start = first_offset
        .checked_sub(3)
        .expect("x86-64 TLSDESC relocation offset is before the LEA instruction displacement");
    let end = call_offset + 11;
    assert!(
        end <= section_len,
        "x86-64 TLSDESC sequence extends beyond section data"
    );
    (start, end)
}

#[cfg(target_arch = "aarch64")]
const AARCH64_INSN_LEN: usize = 4;

/// Instructions to include before the first relocation and after the last relocation when
/// searching the compiler output for the thread-pointer computation on aarch64.
///
/// The `mrs tpidr_el0` and the final `add` are not relocation-bearing, so compilers schedule them
/// freely:
/// - Older gcc (as on the CentOS/Alpine CI) hoists the `mrs` *above* the `adrp` (one or two
///   instructions before the first relocation).
/// - gcc 15.2 keeps the `mrs` after the `blr` but interleaves the function epilogue between it and
///   the final `add`, so the `add` lands three instructions past the `blr`.
/// - clang and our own inline asm keep both right after the `blr`.
///
/// The window is sized to cover all of these; bump [`TP_SEARCH_INSNS_AFTER`] if a future toolchain
/// spreads the computation out further.
#[cfg(target_arch = "aarch64")]
const TP_SEARCH_INSNS_BEFORE: usize = 2;
#[cfg(target_arch = "aarch64")]
const TP_SEARCH_INSNS_AFTER: usize = 4;

/// `mrs Xt, TPIDR_EL0` encodes its destination register in Rt, bits [4:0].
#[cfg(target_arch = "aarch64")]
const AARCH64_MRS_TP_REG_MASK: u32 = 0x0000_001F;
/// `add x0, Xn, x0` encodes its first source register in Rn, bits [9:5].
#[cfg(target_arch = "aarch64")]
const AARCH64_ADD_TP_REG_MASK: u32 = 0x0000_03E0;
/// `ldr Xt, [x0, :tlsdesc_lo12:]` encodes the descriptor register in Rt, bits [4:0].
#[cfg(target_arch = "aarch64")]
const AARCH64_LDR_DESC_REG_MASK: u32 = 0x0000_001F;
/// `blr Xn` encodes the descriptor register in Rn, bits [9:5].
#[cfg(target_arch = "aarch64")]
const AARCH64_BLR_DESC_REG_MASK: u32 = 0x0000_03E0;

/// `mrs Xt, TPIDR_EL0` with its destination register masked out.
#[cfg(target_arch = "aarch64")]
const AARCH64_MRS_TPIDR_EL0_MASKED: u32 = 0xd53b_d040;
/// `add x0, Xn, x0` with its first source register masked out.
#[cfg(target_arch = "aarch64")]
const AARCH64_ADD_TP_MASKED: u32 = 0x8b00_0000;

/// The core TLSDESC sequence on aarch64 is the four relocation-bearing instructions `adrp`, `ldr`,
/// `add` and (`.tlsdesccall`) `blr`. The first relocation is on the `adrp`, the last on the `blr`.
/// The window stops at the `blr`: the trailing `mrs`/`add` are handled separately by
/// [`assert_thread_pointer_computation`] because their placement is not stable across compilers.
#[cfg(target_arch = "aarch64")]
fn tlsdesc_sequence_bounds(relocations: &[Relocation], section_len: usize) -> (usize, usize) {
    let start = usize::try_from(relocations[0].offset)
        .expect("first relocation offset does not fit in usize");
    let last_offset = usize::try_from(relocations[relocations.len() - 1].offset)
        .expect("last relocation offset does not fit in usize");
    let end = last_offset + AARCH64_INSN_LEN;
    assert!(
        end <= section_len,
        "AArch64 TLSDESC core sequence extends beyond section data"
    );
    (start, end)
}

/// Mask the descriptor scratch register out of the core `ldr`/`blr` so the byte comparison ignores
/// which register holds the TLS descriptor.
///
/// gcc reads the thread pointer into `x1` and therefore picks `x2` for the descriptor; clang and
/// our inline asm use `x1`.
#[cfg(target_arch = "aarch64")]
fn mask_aarch64_descriptor_register(core: &mut [u8], relocations: &[Relocation], start: usize) {
    // Relocations are sorted by offset: [ADR_PAGE21, LD64_LO12, ADD_LO12, CALL] => adrp, ldr, add,
    // blr. The descriptor register appears in the `ldr` (second relocation) and the `blr` (last).
    let ldr_offset = usize::try_from(relocations[1].offset)
        .expect("ldr relocation offset does not fit in usize")
        - start;
    let blr_offset = usize::try_from(relocations[relocations.len() - 1].offset)
        .expect("blr relocation offset does not fit in usize")
        - start;
    mask_instruction_bits(core, ldr_offset, AARCH64_LDR_DESC_REG_MASK);
    mask_instruction_bits(core, blr_offset, AARCH64_BLR_DESC_REG_MASK);
}

/// A window of instruction bytes around the relocation-bearing core, used to locate the
/// thread-pointer computation.
#[cfg(target_arch = "aarch64")]
#[derive(Debug)]
struct TpWindow {
    bytes: Vec<u8>,
    /// Offset of the `blr` (last relocation) within `bytes`.
    blr_offset: usize,
}

#[cfg(target_arch = "aarch64")]
fn extract_tp_window(section_data: &[u8], relocations: &[Relocation]) -> TpWindow {
    let first = usize::try_from(relocations[0].offset)
        .expect("first relocation offset does not fit in usize");
    let last = usize::try_from(relocations[relocations.len() - 1].offset)
        .expect("last relocation offset does not fit in usize");
    // Relocation offsets are 4-byte aligned, so subtracting whole instructions keeps the window
    // aligned to instruction boundaries.
    let start = first.saturating_sub(TP_SEARCH_INSNS_BEFORE * AARCH64_INSN_LEN);
    let end = (last + AARCH64_INSN_LEN * (1 + TP_SEARCH_INSNS_AFTER)).min(section_data.len());
    TpWindow {
        bytes: section_data[start..end].to_vec(),
        blr_offset: last - start,
    }
}

/// Clear the bits set in `mask` from the 32-bit little-endian instruction word at `offset`.
#[cfg(target_arch = "aarch64")]
fn mask_instruction_bits(bytes: &mut [u8], offset: usize, mask: u32) {
    let word: [u8; 4] = bytes[offset..offset + 4]
        .try_into()
        .expect("instruction word extends beyond the extracted sequence");
    let masked = u32::from_le_bytes(word) & !mask;
    bytes[offset..offset + 4].copy_from_slice(&masked.to_le_bytes());
}

/// Read the 32-bit little-endian instruction at `offset` with the bits in `mask` cleared.
#[cfg(target_arch = "aarch64")]
fn masked_instruction_at(bytes: &[u8], offset: usize, mask: u32) -> u32 {
    let word: [u8; 4] = bytes[offset..offset + 4]
        .try_into()
        .expect("instruction word extends beyond the extracted window");
    u32::from_le_bytes(word) & !mask
}

/// Find the first instruction at or after `from` (scanning on 4-byte boundaries) whose value, once
/// `mask` is cleared, equals `target`.
#[cfg(target_arch = "aarch64")]
fn find_masked_instruction(bytes: &[u8], from: usize, mask: u32, target: u32) -> Option<usize> {
    (from..bytes.len().saturating_sub(AARCH64_INSN_LEN - 1))
        .step_by(AARCH64_INSN_LEN)
        .find(|&offset| masked_instruction_at(bytes, offset, mask) == target)
}

fn tlsdesc_access_from_relocations(
    section_data: &[u8],
    relocations: &[Relocation],
) -> TlsDescAccess {
    let (start, end) = tlsdesc_sequence_bounds(relocations, section_data.len());
    #[cfg_attr(not(target_arch = "aarch64"), allow(unused_mut))]
    let mut core = section_data[start..end].to_vec();
    #[cfg(target_arch = "aarch64")]
    mask_aarch64_descriptor_register(&mut core, relocations, start);
    let sequence = TlsDescSequence {
        core_instructions: core,
        relocations: relocations
            .iter()
            .map(|relocation| Relocation {
                offset: relocation.offset
                    - u64::try_from(start).expect("could not fit `start` into a u64"),
                relocation_type: relocation.relocation_type,
                addend: relocation.addend,
            })
            .collect(),
    };
    TlsDescAccess {
        sequence,
        #[cfg(target_arch = "aarch64")]
        tp_window: extract_tp_window(section_data, relocations),
    }
}

fn tlsdesc_accesses_for_symbol_in_elf(
    data: &[u8],
    symbol: &str,
    label: &str,
) -> Vec<TlsDescAccess> {
    let elf = parse_elf(data, label);
    let Some(section_headers) = elf.section_headers() else {
        panic!("{label} has no ELF section headers");
    };
    let mut accesses = Vec::new();

    for section_header in section_headers
        .iter()
        .filter(|shdr| matches!(shdr.sh_type, abi::SHT_REL | abi::SHT_RELA))
    {
        let symbol_indexes =
            symbol_indexes_in_table(&elf, section_header.sh_link as usize, symbol, label);
        if symbol_indexes.is_empty() {
            continue;
        }

        let target_header = section_headers
            .get(section_header.sh_info as usize)
            .unwrap_or_else(|e| panic!("failed to read relocation target section header: {e}"));
        let (target_data, _) = elf
            .section_data(&target_header)
            .unwrap_or_else(|e| panic!("failed to read relocation target section in {label}: {e}"));
        let mut relocations = Vec::new();

        match section_header.sh_type {
            abi::SHT_REL => {
                let rels = elf
                    .section_data_as_rels(&section_header)
                    .unwrap_or_else(|e| panic!("failed to read REL relocations in {label}: {e}"));
                relocations.extend(
                    rels.filter(|rel| {
                        symbol_indexes.contains(&rel.r_sym)
                            && is_tlsdesc_object_relocation(RelocationType(rel.r_type))
                    })
                    .map(|rel| Relocation {
                        offset: rel.r_offset,
                        relocation_type: RelocationType(rel.r_type),
                        addend: 0,
                    }),
                );
            }
            abi::SHT_RELA => {
                let relas = elf
                    .section_data_as_relas(&section_header)
                    .unwrap_or_else(|e| panic!("failed to read RELA relocations in {label}: {e}"));
                relocations.extend(
                    relas
                        .filter(|rela| {
                            symbol_indexes.contains(&rela.r_sym)
                                && is_tlsdesc_object_relocation(RelocationType(rela.r_type))
                        })
                        .map(|rela| Relocation {
                            offset: rela.r_offset,
                            relocation_type: RelocationType(rela.r_type),
                            addend: rela.r_addend,
                        }),
                );
            }
            _ => unreachable!(),
        }

        relocations.sort_by_key(|relocation| relocation.offset);
        assert!(
            relocations.len() % TLSDESC_RELOCATIONS_PER_ACCESS == 0,
            "expected TLSDESC relocations for {symbol} in {label} to come in groups of \
             {TLSDESC_RELOCATIONS_PER_ACCESS}; found {relocations:?}"
        );

        accesses.extend(
            relocations
                .chunks_exact(TLSDESC_RELOCATIONS_PER_ACCESS)
                .map(|chunk| tlsdesc_access_from_relocations(target_data, chunk)),
        );
    }

    accesses
}

fn tlsdesc_accesses_for_symbol_in_file(path: &Path, symbol: &str) -> Vec<TlsDescAccess> {
    let data =
        std::fs::read(path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    tlsdesc_accesses_for_symbol_in_elf(&data, symbol, &path.display().to_string())
}

fn archive_tlsdesc_accesses_for_symbol(path: &Path, symbol: &str) -> Vec<(String, TlsDescAccess)> {
    let mut accesses = Vec::new();

    for_each_archive_elf_member(path, |member_name, member_data| {
        let label = format!("{}({member_name})", path.display());
        accesses.extend(
            tlsdesc_accesses_for_symbol_in_elf(member_data, symbol, &label)
                .into_iter()
                .map(|access| (member_name.to_owned(), access)),
        );
    });

    accesses
}

/// Verify the compiler output performs the same thread-pointer computation our inline asm does: an
/// `mrs Xn, TPIDR_EL0` followed, in program order (possibly with unrelated instructions in between
/// — gcc may hoist the `mrs` and/or interleave the epilogue), by an `add x0, Xn, x0`. Both are
/// matched up to the scratch register, which the compiler is free to choose.
#[cfg(target_arch = "aarch64")]
fn assert_thread_pointer_computation(
    rust: &TpWindow,
    c: &TpWindow,
    rust_label: &str,
    c_label: &str,
) {
    // Our inline asm emits the `mrs` and `add` as the two instructions right after the `blr`.
    let mrs = masked_instruction_at(
        &rust.bytes,
        rust.blr_offset + AARCH64_INSN_LEN,
        AARCH64_MRS_TP_REG_MASK,
    );
    let add = masked_instruction_at(
        &rust.bytes,
        rust.blr_offset + 2 * AARCH64_INSN_LEN,
        AARCH64_ADD_TP_REG_MASK,
    );
    // Guard against our own asm drifting away from the shape this search assumes.
    assert_eq!(
        mrs, AARCH64_MRS_TPIDR_EL0_MASKED,
        "expected our inline asm to read `tpidr_el0` right after the TLSDESC call in {rust_label}"
    );
    assert_eq!(
        add, AARCH64_ADD_TP_MASKED,
        "expected our inline asm to `add x0, <tp>, x0` right after reading the thread pointer in \
         {rust_label}"
    );

    let mrs_pos = find_masked_instruction(&c.bytes, 0, AARCH64_MRS_TP_REG_MASK, mrs)
        .unwrap_or_else(|| {
            panic!(
                "no `mrs Xn, tpidr_el0` found near the TLSDESC access in {c_label}: the compiler's \
                 thread-pointer read does not match our inline asm. Set \
                 {SKIP_TLS_SHIM_ASM_TEST_ENV}=1 to skip this guard with a different local compiler."
            )
        });
    find_masked_instruction(
        &c.bytes,
        mrs_pos + AARCH64_INSN_LEN,
        AARCH64_ADD_TP_REG_MASK,
        add,
    )
    .unwrap_or_else(|| {
        panic!(
            "no `add x0, Xn, x0` after the `tpidr_el0` read near the TLSDESC access in {c_label}: \
             the compiler's thread-pointer computation does not match our inline asm. Set \
             {SKIP_TLS_SHIM_ASM_TEST_ENV}=1 to skip this guard with a different local compiler."
        )
    });
}

fn compile_tls_shim_object(dir: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/tls_shim.c");
    let object = dir.join("tls_shim.o");
    let mut compile_object = Command::new("cc");
    compile_object.args(["-O2", "-fPIC", "-fomit-frame-pointer", "-c"]);

    #[cfg(target_arch = "x86_64")]
    compile_object.arg("-mtls-dialect=gnu2");

    compile_object.arg(&source).arg("-o").arg(&object);
    assert_command_success(&mut compile_object);
    object
}

#[test]
#[cfg_attr(miri, ignore)]
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn tlsdesc_inline_assembly_matches_c_compiler_sequence() {
    fn print_skip_msg(label: &str) {
        eprintln!("WARNING: {label}. Skipping inline assembly matches C compiler sequence test");
    }

    if !native_target() {
        print_skip_msg("cross-compilation detected");
        return;
    }
    if skip_tls_shim_asm_test() {
        print_skip_msg(&format!(
            "{SKIP_TLS_SHIM_ASM_TEST_ENV} environment varialbe set"
        ));
        return;
    }

    if !required_tools_available(&["cc"]) {
        print_skip_msg("no C compiler available");
        return;
    }

    let staticlib = staticlib_path();
    check_readable(&staticlib);

    let dir = build_dir("otel-thread-ctx-tls-shim");
    let c_object = compile_tls_shim_object(&dir);
    let c_accesses = tlsdesc_accesses_for_symbol_in_file(&c_object, SYMBOL);
    assert_eq!(
        c_accesses.len(),
        1,
        "expected one compiler-generated TLSDESC access in {}; found {}. \
         Set {SKIP_TLS_SHIM_ASM_TEST_ENV}=1 to skip this guard with a different local compiler.",
        c_object.display(),
        c_accesses.len()
    );
    let expected = &c_accesses[0];

    let rust_accesses = archive_tlsdesc_accesses_for_symbol(&staticlib, SYMBOL);
    assert!(
        !rust_accesses.is_empty(),
        "expected at least one Rust inline-asm TLSDESC access for {SYMBOL} in {}",
        staticlib.display()
    );

    for (member_name, access) in rust_accesses {
        // The relocation-bearing core the linker relaxes must match byte-for-byte (up to the
        // descriptor scratch register, which relaxation discards).
        assert_eq!(
            &access.sequence,
            &expected.sequence,
            "Rust inline assembly TLSDESC core in {}({member_name}) does not match compiler output \
             from {}. Set {SKIP_TLS_SHIM_ASM_TEST_ENV}=1 to skip this guard with a different local \
             compiler.",
            staticlib.display(),
            c_object.display()
        );
        // The thread-pointer computation is not relocation-bearing; compilers schedule it freely,
        // so we only require the same instructions to appear in order near the core (aarch64).
        #[cfg(target_arch = "aarch64")]
        assert_thread_pointer_computation(
            &access.tp_window,
            &expected.tp_window,
            &format!("{}({member_name})", staticlib.display()),
            &c_object.display().to_string(),
        );
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn otel_thread_ctx_v1_tls_properties() {
    let path = cdylib_path();
    check_readable(&path);
    libdd_otel_thread_ctx::sanity_check::check_tls_slot_in(&path).unwrap();
}
