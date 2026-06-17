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
//! - A native executable that statically links libdd-otel-thread-ctx-ffi without exporting
//!   `otel_thread_ctx_v1` has libdd's TLSDESC access relaxed to local-exec TLS, leaving no
//!   relocation for `otel_thread_ctx_v1`.
//!
//! Library artifact paths are derived at runtime from the test executable location.
//! The test binary and crate artifacts live in `target/<[triple/]profile>/deps/`.

#![cfg(target_os = "linux")]

use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use elf::{abi, endian::AnyEndian, symbol::SymbolTable, ElfBytes};

const SYMBOL: &str = "otel_thread_ctx_v1";

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

fn command_output(command: &mut Command) -> String {
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
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn objdump(args: &[&str], path: &Path) -> String {
    let mut command = Command::new("objdump");
    command.args(args).arg(path);
    command_output(&mut command)
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

fn relocation_types_for_symbol_in_elf(data: &[u8], symbol: &str, label: &str) -> Vec<u32> {
    let elf = parse_elf(data, label);
    let Some(section_headers) = elf.section_headers() else {
        panic!("{label} has no ELF section headers");
    };
    let mut relocation_types = Vec::new();

    for section_header in section_headers
        .iter()
        .filter(|shdr| matches!(shdr.sh_type, abi::SHT_REL | abi::SHT_RELA))
    {
        let symbol_indexes =
            symbol_indexes_in_table(&elf, section_header.sh_link as usize, symbol, label);
        if symbol_indexes.is_empty() {
            continue;
        }

        match section_header.sh_type {
            abi::SHT_REL => {
                let rels = elf
                    .section_data_as_rels(&section_header)
                    .unwrap_or_else(|e| panic!("failed to read REL relocations in {label}: {e}"));
                relocation_types.extend(
                    rels.filter(|rel| symbol_indexes.contains(&rel.r_sym))
                        .map(|rel| rel.r_type),
                );
            }
            abi::SHT_RELA => {
                let relas = elf
                    .section_data_as_relas(&section_header)
                    .unwrap_or_else(|e| panic!("failed to read RELA relocations in {label}: {e}"));
                relocation_types.extend(
                    relas
                        .filter(|rela| symbol_indexes.contains(&rela.r_sym))
                        .map(|rela| rela.r_type),
                );
            }
            _ => unreachable!(),
        }
    }

    relocation_types
}

fn relocation_types_for_symbol_in_file(path: &Path, symbol: &str) -> Vec<u32> {
    let data =
        std::fs::read(path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    relocation_types_for_symbol_in_elf(&data, symbol, &path.display().to_string())
}

fn parse_ascii_usize(bytes: &[u8], what: &str) -> usize {
    std::str::from_utf8(bytes)
        .unwrap_or_else(|e| panic!("invalid UTF-8 in {what}: {e}"))
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("failed to parse {what}: {e}"))
}

fn trim_archive_name(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim()
        .trim_end_matches('/')
        .to_owned()
}

fn gnu_archive_name(name_table: &[u8], offset: usize) -> String {
    assert!(
        offset < name_table.len(),
        "GNU archive name offset {offset} is outside the name table"
    );
    let rest = &name_table[offset..];
    let end = rest.iter().position(|b| *b == b'\n').unwrap_or(rest.len());
    trim_archive_name(&rest[..end])
}

fn archive_member_name_and_data<'a>(
    name_field: &[u8],
    member: &'a [u8],
    gnu_name_table: Option<&'a [u8]>,
) -> (String, &'a [u8]) {
    let name = std::str::from_utf8(name_field)
        .unwrap_or_else(|e| panic!("invalid UTF-8 in archive member name: {e}"))
        .trim();

    if matches!(name, "/" | "//") {
        return (name.to_owned(), member);
    }

    if let Some(name_len) = name.strip_prefix("#1/") {
        let name_len = name_len
            .parse::<usize>()
            .unwrap_or_else(|e| panic!("failed to parse BSD archive name length: {e}"));
        assert!(
            name_len <= member.len(),
            "BSD archive member name length {name_len} exceeds member data length {}",
            member.len()
        );
        return (trim_archive_name(&member[..name_len]), &member[name_len..]);
    }

    if let Some(offset) = name.strip_prefix('/') {
        if !offset.is_empty() && offset.bytes().all(|b| b.is_ascii_digit()) {
            let offset = offset
                .parse::<usize>()
                .unwrap_or_else(|e| panic!("failed to parse GNU archive name offset: {e}"));
            let gnu_name_table =
                gnu_name_table.expect("GNU archive name offset used before the name table");
            return (gnu_archive_name(gnu_name_table, offset), member);
        }
    }

    (trim_archive_name(name_field), member)
}

fn archive_relocation_types_for_symbol(path: &Path, symbol: &str) -> Vec<(String, Vec<u32>)> {
    const ARMAG: &[u8] = b"!<arch>\n";
    const HEADER_LEN: usize = 60;

    let archive =
        std::fs::read(path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    assert!(
        archive.starts_with(ARMAG),
        "{} is not an ar archive",
        path.display()
    );

    let mut offset = ARMAG.len();
    let mut gnu_name_table = None;
    let mut relocations = Vec::new();

    while offset < archive.len() {
        assert!(
            offset + HEADER_LEN <= archive.len(),
            "truncated ar header in {} at offset {offset}",
            path.display()
        );
        let header = &archive[offset..offset + HEADER_LEN];
        assert_eq!(
            &header[58..60],
            b"`\n",
            "invalid ar header trailer in {} at offset {offset}",
            path.display()
        );
        offset += HEADER_LEN;

        let member_size = parse_ascii_usize(&header[48..58], "archive member size");
        let member_end = offset
            .checked_add(member_size)
            .expect("archive member end offset overflowed");
        assert!(
            member_end <= archive.len(),
            "truncated ar member in {} at offset {offset}",
            path.display()
        );

        let member = &archive[offset..member_end];
        let (member_name, member_data) =
            archive_member_name_and_data(&header[0..16], member, gnu_name_table);

        if member_name == "//" {
            gnu_name_table = Some(member);
        } else if member_data.starts_with(&abi::ELFMAGIC) {
            let label = format!("{}({member_name})", path.display());
            let relocation_types = relocation_types_for_symbol_in_elf(member_data, symbol, &label);
            if !relocation_types.is_empty() {
                relocations.push((member_name.clone(), relocation_types));
            }
        }

        offset = member_end + member_size % 2;
        assert!(
            offset <= archive.len(),
            "truncated ar padding in {} after member {member_name}",
            path.display()
        );
    }

    relocations
}

#[cfg(target_arch = "x86_64")]
fn is_tlsdesc_object_relocation(relocation_type: u32) -> bool {
    // These are object-file TLSDESC relocations. `R_X86_64_TLSDESC` is the dynamic-linker
    // relocation emitted after linking, so it is intentionally excluded here.
    matches!(
        relocation_type,
        abi::R_X86_64_GOTPC32_TLSDESC | abi::R_X86_64_TLSDESC_CALL
    )
}

#[cfg(target_arch = "aarch64")]
fn is_tlsdesc_object_relocation(relocation_type: u32) -> bool {
    // These are object-file TLSDESC relocations. `R_AARCH64_TLSDESC` is the dynamic-linker
    // relocation emitted after linking, so it is intentionally excluded here.
    matches!(
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
    )
}

fn format_relocations(relocations: &[(String, Vec<u32>)]) -> String {
    if relocations.is_empty() {
        return "<none>".to_owned();
    }

    relocations
        .iter()
        .map(|(name, types)| format!("{name}: {types:?}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_disassembly_header_for(line: &str, name: &str) -> bool {
    let Some((_, symbol)) = line.split_once('<') else {
        return false;
    };
    let Some(symbol) = symbol.strip_suffix(">:") else {
        return false;
    };
    symbol == name
        || symbol
            .strip_prefix(name)
            .is_some_and(|suffix| suffix.starts_with("::"))
}

fn disassembled_functions(output: &str, name: &str) -> Vec<String> {
    let mut functions = Vec::new();
    let mut current_function = Vec::new();

    for line in output.lines() {
        if is_disassembly_header_for(line, name) {
            if !current_function.is_empty() {
                functions.push(current_function.join("\n"));
                current_function.clear();
            }
            current_function.push(line);
            continue;
        }

        if !current_function.is_empty() {
            if line.is_empty() {
                functions.push(current_function.join("\n"));
                current_function.clear();
                continue;
            }
            current_function.push(line);
        }
    }

    if !current_function.is_empty() {
        functions.push(current_function.join("\n"));
    }

    assert!(
        !functions.is_empty(),
        "could not find disassembly for {name} in:\n{output}"
    );
    functions
}

#[cfg(target_arch = "aarch64")]
fn disassembly_window_around_line(
    function: &str,
    needle: &str,
    before: usize,
    after: usize,
) -> String {
    let lines = function.lines().collect::<Vec<_>>();
    let line_index = lines
        .iter()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("could not find {needle:?} in:\n{function}"));
    let start = line_index.saturating_sub(before);
    let end = usize::min(line_index + after + 1, lines.len());
    lines[start..end].join("\n")
}

#[test]
#[cfg_attr(miri, ignore)]
fn otel_thread_ctx_v1_tls_properties() {
    let path = cdylib_path();
    check_readable(&path);
    libdd_otel_thread_ctx::sanity_check::check_tls_slot_in(&path).unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn statically_linked_executable_relaxes_libdd_tls_slot_to_local_exec() {
    if !native_target() {
        return;
    }

    if !required_tools_available(&["cc", "objdump"]) {
        return;
    }

    let staticlib = staticlib_path();
    check_readable(&staticlib);

    let dir = build_dir("otel-thread-ctx-local-exec");
    let source = dir.join("consumer.c");
    let object = dir.join("consumer.o");
    let executable = dir.join("consumer");
    std::fs::write(
        &source,
        r#"
#include <stdint.h>

void ddog_otel_thread_ctx_update(
    const uint8_t (*trace_id)[16],
    const uint8_t (*span_id)[8],
    const uint8_t (*local_root_span_id)[8]);
void *ddog_otel_thread_ctx_detach(void);
void ddog_otel_thread_ctx_free(void *ctx);

int main(void) {
    uint8_t trace_id[16] = {1};
    uint8_t span_id[8] = {2};
    uint8_t local_root_span_id[8] = {3};

    ddog_otel_thread_ctx_update(&trace_id, &span_id, &local_root_span_id);
    void *ctx = ddog_otel_thread_ctx_detach();
    ddog_otel_thread_ctx_free(ctx);

    return ctx == 0 ? 1 : 0;
}
"#,
    )
    .unwrap_or_else(|e| panic!("failed to write {}: {e}", source.display()));

    let mut compile_object = Command::new("cc");
    compile_object.args(["-O2", "-ffunction-sections", "-fdata-sections"]);
    compile_object.arg("-c").arg(&source).arg("-o").arg(&object);
    assert_command_success(&mut compile_object);

    let staticlib_relocations = archive_relocation_types_for_symbol(&staticlib, SYMBOL);
    assert!(
        staticlib_relocations
            .iter()
            .any(|(_, types)| types.iter().any(|t| is_tlsdesc_object_relocation(*t))),
        "expected an object-file TLSDESC relocation for {SYMBOL} in {}\nfound:\n{}",
        staticlib.display(),
        format_relocations(&staticlib_relocations)
    );

    let object_relocations = relocation_types_for_symbol_in_file(&object, SYMBOL);
    assert!(
        object_relocations.is_empty(),
        "expected generated C object to have no relocations for {SYMBOL}; found {object_relocations:?}"
    );

    let mut link_executable = Command::new("cc");
    link_executable
        .arg(&object)
        .arg(&staticlib)
        .args([
            "-Wl,--gc-sections",
            "-lpthread",
            "-ldl",
            "-lm",
            "-lrt",
            "-lutil",
        ])
        .arg("-o")
        .arg(&executable);
    assert_command_success(&mut link_executable);

    // Run the generated executable so the test validates the relaxed TLS access at runtime too.
    let mut run_executable = Command::new(&executable);
    assert_command_success(&mut run_executable);

    let executable_relocations = relocation_types_for_symbol_in_file(&executable, SYMBOL);
    assert!(
        executable_relocations.is_empty(),
        "expected no remaining relocations for {SYMBOL} in {}; found {executable_relocations:?}",
        executable.display()
    );

    let disassembly = objdump(&["-drwC"], &executable);
    let tls_slot_functions =
        disassembled_functions(&disassembly, "libdd_otel_thread_ctx::linux::with_tls_slot");

    #[cfg(target_arch = "x86_64")]
    {
        assert!(
            tls_slot_functions
                .iter()
                .any(|function| function.contains("%fs:0x0")),
            "expected tls_slot() in libdd-otel-thread-ctx to be relaxed to local-exec x86-64 \
             TLS access through %fs:0x0\n{}",
            tls_slot_functions.join("\n\n")
        );
        assert!(
            tls_slot_functions
                .iter()
                .all(|function| !function.contains("tlsdesc")),
            "expected linker-relaxed local-exec TLS code without TLSDESC operands:\n{}",
            tls_slot_functions.join("\n\n")
        );
    }

    #[cfg(target_arch = "aarch64")]
    {
        let function = tls_slot_functions
            .iter()
            .find(|function| function.contains("tpidr_el0"))
            .unwrap_or_else(|| {
                panic!(
                    "expected tls_slot() in libdd-otel-thread-ctx to use tpidr_el0 after \
                     relaxation\n{}",
                    tls_slot_functions.join("\n\n")
                )
            });
        let window = disassembly_window_around_line(function, "tpidr_el0", 4, 3);
        assert!(
            !window.contains("tlsdesc") && !window.contains("\tblr"),
            "expected linker-relaxed local-exec TLS code around tpidr_el0 without a TLSDESC call:\n\
             {window}"
        );
    }
}
