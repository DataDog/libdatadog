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
//! - The Rust inline-asm TLSDESC access sequence byte-for-byte matches what a C compiler generates
//!   (guaranteeing that linker TLS relaxation works identically to a compiler-generated access).
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
struct TlsDescRelocation {
    offset: usize,
    relocation_type: RelocationType,
    addend: i64,
}

#[derive(Debug, PartialEq, Eq)]
struct TlsDescSequence {
    bytes: Vec<u8>,
    relocations: Vec<TlsDescRelocation>,
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

#[derive(Debug)]
struct RawRelocation {
    offset: u64,
    relocation_type: RelocationType,
    addend: i64,
}

#[cfg(target_arch = "x86_64")]
const TLSDESC_RELOCATIONS_PER_ACCESS: usize = 2;

#[cfg(target_arch = "aarch64")]
const TLSDESC_RELOCATIONS_PER_ACCESS: usize = 4;

#[cfg(target_arch = "x86_64")]
fn tlsdesc_sequence_bounds(relocations: &[RawRelocation], section_len: usize) -> (usize, usize) {
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
fn tlsdesc_sequence_bounds(relocations: &[RawRelocation], section_len: usize) -> (usize, usize) {
    let first_offset = usize::try_from(relocations[0].offset)
        .expect("first relocation offset does not fit in usize");
    let start = first_offset
        .checked_sub(4)
        .expect("AArch64 TLSDESC relocation offset is before the TPIDR_EL0 read");
    let last_offset = usize::try_from(relocations[relocations.len() - 1].offset)
        .expect("last relocation offset does not fit in usize");
    let end = last_offset + 8;
    assert!(
        end <= section_len,
        "AArch64 TLSDESC sequence extends beyond section data"
    );
    (start, end)
}

fn tlsdesc_sequence_from_relocations(
    section_data: &[u8],
    relocations: &[RawRelocation],
) -> TlsDescSequence {
    let (start, end) = tlsdesc_sequence_bounds(relocations, section_data.len());
    TlsDescSequence {
        bytes: section_data[start..end].to_vec(),
        relocations: relocations
            .iter()
            .map(|relocation| TlsDescRelocation {
                offset: usize::try_from(relocation.offset)
                    .expect("relocation offset does not fit in usize")
                    - start,
                relocation_type: relocation.relocation_type,
                addend: relocation.addend,
            })
            .collect(),
    }
}

fn tlsdesc_sequences_for_symbol_in_elf(
    data: &[u8],
    symbol: &str,
    label: &str,
) -> Vec<TlsDescSequence> {
    let elf = parse_elf(data, label);
    let Some(section_headers) = elf.section_headers() else {
        panic!("{label} has no ELF section headers");
    };
    let mut sequences = Vec::new();

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
                    .map(|rel| RawRelocation {
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
                        .map(|rela| RawRelocation {
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

        sequences.extend(
            relocations
                .chunks_exact(TLSDESC_RELOCATIONS_PER_ACCESS)
                .map(|chunk| tlsdesc_sequence_from_relocations(target_data, chunk)),
        );
    }

    sequences
}

fn tlsdesc_sequences_for_symbol_in_file(path: &Path, symbol: &str) -> Vec<TlsDescSequence> {
    let data =
        std::fs::read(path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    tlsdesc_sequences_for_symbol_in_elf(&data, symbol, &path.display().to_string())
}

fn archive_tlsdesc_sequences_for_symbol(
    path: &Path,
    symbol: &str,
) -> Vec<(String, TlsDescSequence)> {
    let mut sequences = Vec::new();

    for_each_archive_elf_member(path, |member_name, member_data| {
        let label = format!("{}({member_name})", path.display());
        sequences.extend(
            tlsdesc_sequences_for_symbol_in_elf(member_data, symbol, &label)
                .into_iter()
                .map(|sequence| (member_name.to_owned(), sequence)),
        );
    });

    sequences
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
    if !native_target() || skip_tls_shim_asm_test() {
        return;
    }

    if !required_tools_available(&["cc"]) {
        return;
    }

    let staticlib = staticlib_path();
    check_readable(&staticlib);

    let dir = build_dir("otel-thread-ctx-tls-shim");
    let c_object = compile_tls_shim_object(&dir);
    let c_sequences = tlsdesc_sequences_for_symbol_in_file(&c_object, SYMBOL);
    assert_eq!(
        c_sequences.len(),
        1,
        "expected one compiler-generated TLSDESC access in {}; found {c_sequences:?}. \
         Set {SKIP_TLS_SHIM_ASM_TEST_ENV}=1 to skip this guard with a different local compiler.",
        c_object.display()
    );
    let expected = &c_sequences[0];

    let rust_sequences = archive_tlsdesc_sequences_for_symbol(&staticlib, SYMBOL);
    assert!(
        !rust_sequences.is_empty(),
        "expected at least one Rust inline-asm TLSDESC access for {SYMBOL} in {}",
        staticlib.display()
    );

    for (member_name, sequence) in rust_sequences {
        assert_eq!(
            &sequence,
            expected,
            "Rust inline assembly TLSDESC sequence in {}({member_name}) does not match \
             compiler output from {}. Set {SKIP_TLS_SHIM_ASM_TEST_ENV}=1 to skip this guard with \
             a different local compiler.",
            staticlib.display(),
            c_object.display()
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
