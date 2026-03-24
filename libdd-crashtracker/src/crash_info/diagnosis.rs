// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::memory_map::{MemoryMap, MemoryMapping};
use super::registers::Registers;
use super::sig_info::{SiCodes, SigInfo, SignalNames};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const NULL_PAGE_THRESHOLD: u64 = 0x10000; // 64KB
const STACK_GUARD_DISTANCE: u64 = 0x2000; // 8KB — two guard pages

/// Category of the diagnosed crash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum DiagnosisCategory {
    NullPointerDereference,
    StackOverflow,
    UseAfterFree,
    WriteToReadOnly,
    ExecuteNonExecutable,
    MisalignedAccess,
    IllegalInstruction,
    IntentionalAbort,
    WildPointer,
    Unknown,
}

impl std::fmt::Display for DiagnosisCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// Information about a memory mapping that an address falls in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MappingInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub permissions: String,
    pub offset_in_mapping: String,
}

/// Structured crash diagnosis produced by correlating signal info, CPU
/// registers, and the process memory map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CrashDiagnosis {
    pub summary: String,
    pub category: DiagnosisCategory,
    pub details: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fault_address_mapped: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fault_address_mapping: Option<MappingInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crash_location: Option<MappingInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_pointer_valid: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub null_registers: Vec<String>,
}

/// Parse a hex address string (with or without "0x" prefix) into a u64.
pub fn parse_hex_addr(s: &str) -> Option<u64> {
    let stripped = s.trim_start_matches("0x").trim_start_matches("0X");
    u64::from_str_radix(stripped, 16).ok()
}

/// Top-level diagnosis entry point.
///
/// Correlates signal info, CPU register state, and the process memory map to
/// produce a structured crash diagnosis. All three inputs are required —
/// the caller (`run_diagnosis`) gates invocation on their availability.
pub fn diagnose(
    sig_info: &SigInfo,
    registers: &Registers,
    memory_map: &MemoryMap,
) -> CrashDiagnosis {
    let fault_addr = sig_info.si_addr.as_deref().and_then(parse_hex_addr);
    let ip = registers.ip;

    // Build crash_location from IP
    let crash_location = ip
        .and_then(|ip_val| memory_map.find_mapping(ip_val).map(|mm| (ip_val, mm)))
        .map(|(ip_val, mm)| build_mapping_info(mm, ip_val));

    // Build fault_address info
    let fault_mapping = fault_addr.and_then(|a| memory_map.find_mapping(a).map(|mm| (a, mm)));
    let fault_address_mapped = fault_addr.map(|a| memory_map.find_mapping(a).is_some());
    let fault_address_mapping = fault_mapping.map(|(addr, mm)| build_mapping_info(mm, addr));

    // Check stack pointer validity
    let stack_pointer_valid = registers.sp.map(|sp| {
        memory_map
            .find_stack()
            .map(|stack| sp >= stack.start && sp < stack.end)
            .unwrap_or(false)
    });

    // Near-null registers
    let null_registers = registers.near_null_registers();

    // Run the signal-specific decision tree
    let (summary, category, details) = match sig_info.si_signo_human_readable {
        SignalNames::SIGSEGV => diagnose_sigsegv(
            &sig_info.si_code_human_readable,
            fault_addr,
            fault_mapping.map(|(_, mm)| mm),
            registers,
            memory_map,
            &crash_location,
        ),
        SignalNames::SIGBUS => diagnose_sigbus(
            &sig_info.si_code_human_readable,
            fault_addr,
            fault_mapping.map(|(_, mm)| mm),
        ),
        SignalNames::SIGABRT => (
            "Intentional abort".to_string(),
            DiagnosisCategory::IntentionalAbort,
            "SIGABRT received. Typically caused by assert(), panic!(), abort(), \
             or allocator-detected corruption (double free, heap buffer overflow)."
                .to_string(),
        ),
        SignalNames::SIGILL => diagnose_sigill(ip, memory_map, &crash_location),
        _ => (
            format!(
                "Process terminated by {:?}",
                sig_info.si_signo_human_readable
            ),
            DiagnosisCategory::Unknown,
            format!(
                "Signal {:?} (code {:?}) received.",
                sig_info.si_signo_human_readable, sig_info.si_code_human_readable
            ),
        ),
    };

    CrashDiagnosis {
        summary,
        category,
        details,
        fault_address_mapped,
        fault_address_mapping,
        crash_location,
        stack_pointer_valid,
        null_registers,
    }
}

fn diagnose_sigsegv(
    si_code: &SiCodes,
    fault_addr: Option<u64>,
    fault_mapping: Option<&MemoryMapping>,
    registers: &Registers,
    memory_map: &MemoryMap,
    crash_location: &Option<MappingInfo>,
) -> (String, DiagnosisCategory, String) {
    let addr = match fault_addr {
        Some(a) => a,
        None => {
            return (
                "Segmentation fault (unknown address)".to_string(),
                DiagnosisCategory::Unknown,
                "SIGSEGV received but fault address (si_addr) is not available.".to_string(),
            )
        }
    };

    match si_code {
        SiCodes::SEGV_MAPERR => {
            // Address not mapped at all
            if addr < NULL_PAGE_THRESHOLD {
                let location_detail = crash_location
                    .as_ref()
                    .and_then(|cl| cl.path.as_deref())
                    .unwrap_or("unknown location");
                return (
                    format!(
                        "Null pointer dereference{}",
                        if addr > 0 {
                            format!(" (field offset {:#x})", addr)
                        } else {
                            String::new()
                        }
                    ),
                    DiagnosisCategory::NullPointerDereference,
                    format!(
                        "SIGSEGV (SEGV_MAPERR) at address {:#018x}. Address is below the \
                         null-page threshold ({:#x}), suggesting {} on a null pointer. \
                         Crash in {}.",
                        addr,
                        NULL_PAGE_THRESHOLD,
                        if addr == 0 {
                            "a direct null dereference"
                        } else {
                            "a field or array access"
                        },
                        location_detail,
                    ),
                );
            }

            // Check for stack overflow: fault addr or SP near stack guard
            if let Some(stack) = memory_map.find_stack() {
                let sp = registers.sp;
                let near_stack_bottom =
                    addr < stack.start && stack.start.saturating_sub(addr) <= STACK_GUARD_DISTANCE;
                let sp_near_bottom = sp
                    .map(|sp_val| {
                        sp_val < stack.start
                            || (sp_val >= stack.start
                                && sp_val.saturating_sub(stack.start) <= STACK_GUARD_DISTANCE)
                    })
                    .unwrap_or(false);

                if near_stack_bottom || sp_near_bottom {
                    return (
                        "Stack overflow".to_string(),
                        DiagnosisCategory::StackOverflow,
                        format!(
                            "SIGSEGV (SEGV_MAPERR) at address {:#018x}. Fault address is \
                             near the stack guard page (stack mapping: {:#x}-{:#x}{}). \
                             Stack exhaustion detected.",
                            addr,
                            stack.start,
                            stack.end,
                            sp.map(|s| format!(", SP={:#018x}", s)).unwrap_or_default(),
                        ),
                    );
                }
            }

            // Check if address is near heap
            if let Some(heap) = memory_map.find_heap() {
                let near_heap = addr >= heap.start && addr < heap.end.saturating_add(0x100000); // 1MB after heap end
                if near_heap {
                    return (
                        "Possible use-after-free or heap-adjacent invalid access".to_string(),
                        DiagnosisCategory::UseAfterFree,
                        format!(
                            "SIGSEGV (SEGV_MAPERR) at address {:#018x}. Address is in the \
                             heap neighborhood (heap mapping: {:#x}-{:#x}) but not currently \
                             mapped, suggesting use-after-free or heap corruption.",
                            addr, heap.start, heap.end,
                        ),
                    );
                }
            }

            // Generic unmapped access
            (
                "Wild pointer access".to_string(),
                DiagnosisCategory::WildPointer,
                format!(
                    "SIGSEGV (SEGV_MAPERR) at address {:#018x}. Address is not in any \
                     mapped memory region.",
                    addr,
                ),
            )
        }
        SiCodes::SEGV_ACCERR => {
            // Address is mapped but permissions are wrong
            if let Some(mapping) = fault_mapping {
                if !mapping.writable {
                    return (
                        format!(
                            "Execution without permissions to mapped memory{}",
                            mapping
                                .pathname
                                .as_ref()
                                .map(|p| format!(" in {}", p))
                                .unwrap_or_default()
                        ),
                        DiagnosisCategory::WriteToReadOnly,
                        format!(
                            "SIGSEGV (SEGV_ACCERR) at address {:#018x}. Address is in a \
                             non-writable mapping ({}) of {}.",
                            addr,
                            mapping.permissions_string(),
                            mapping.pathname.as_deref().unwrap_or("anonymous mapping"),
                        ),
                    );
                }
            }
            (
                "Permission violation".to_string(),
                DiagnosisCategory::Unknown,
                format!(
                    "SIGSEGV (SEGV_ACCERR) at address {:#018x}. Address is mapped but \
                     access was denied.",
                    addr,
                ),
            )
        }
        _ => (
            "Segmentation fault".to_string(),
            DiagnosisCategory::Unknown,
            format!("SIGSEGV (code {:?}) at address {:#018x}.", si_code, addr,),
        ),
    }
}

fn diagnose_sigbus(
    si_code: &SiCodes,
    fault_addr: Option<u64>,
    fault_mapping: Option<&MemoryMapping>,
) -> (String, DiagnosisCategory, String) {
    let addr_str = fault_addr
        .map(|a| format!("{:#018x}", a))
        .unwrap_or_else(|| "unknown".to_string());

    match si_code {
        SiCodes::BUS_ADRALN => (
            "Misaligned memory access".to_string(),
            DiagnosisCategory::MisalignedAccess,
            format!(
                "SIGBUS (BUS_ADRALN) at address {}. CPU detected an unaligned \
                 memory access.",
                addr_str,
            ),
        ),
        SiCodes::BUS_ADRERR => {
            let mapping_detail = fault_mapping
                .and_then(|m| {
                    m.pathname
                        .as_ref()
                        .map(|p| format!(" in file-backed mapping of {}", p))
                })
                .unwrap_or_default();
            (
                "Bus error (address error)".to_string(),
                DiagnosisCategory::Unknown,
                format!(
                    "SIGBUS (BUS_ADRERR) at address {}{}. Likely access beyond \
                     end of a memory-mapped file.",
                    addr_str, mapping_detail,
                ),
            )
        }
        _ => (
            "Bus error".to_string(),
            DiagnosisCategory::Unknown,
            format!("SIGBUS (code {:?}) at address {}.", si_code, addr_str),
        ),
    }
}

fn diagnose_sigill(
    ip: Option<u64>,
    memory_map: &MemoryMap,
    crash_location: &Option<MappingInfo>,
) -> (String, DiagnosisCategory, String) {
    if let Some(ip_val) = ip {
        if let Some(mapping) = memory_map.find_mapping(ip_val) {
            if mapping.executable {
                return (
                    "Illegal instruction".to_string(),
                    DiagnosisCategory::IllegalInstruction,
                    format!(
                        "SIGILL at {:#018x} in executable region ({}) of {}. \
                         CPU encountered an invalid opcode - possible ABI mismatch, \
                         compiler bug, or corrupted code section.",
                        ip_val,
                        mapping.permissions_string(),
                        mapping.pathname.as_deref().unwrap_or("anonymous"),
                    ),
                );
            } else {
                return (
                    "Execution of non-executable memory".to_string(),
                    DiagnosisCategory::ExecuteNonExecutable,
                    format!(
                        "SIGILL at {:#018x} in non-executable region ({}) of {}. \
                         Likely a corrupted return address, vtable, or function pointer.",
                        ip_val,
                        mapping.permissions_string(),
                        mapping.pathname.as_deref().unwrap_or("anonymous"),
                    ),
                );
            }
        }
    }

    let location_detail = crash_location
        .as_ref()
        .and_then(|cl| cl.path.as_deref())
        .map(|p| format!(" in {}", p))
        .unwrap_or_default();

    (
        "Illegal instruction".to_string(),
        DiagnosisCategory::IllegalInstruction,
        format!(
            "SIGILL received{}. CPU encountered an invalid opcode.",
            location_detail,
        ),
    )
}

fn build_mapping_info(mapping: &MemoryMapping, addr: u64) -> MappingInfo {
    MappingInfo {
        path: mapping.pathname.clone(),
        permissions: mapping.permissions_string(),
        offset_in_mapping: format!("{:#x}", addr.saturating_sub(mapping.start) + mapping.offset),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crash_info::memory_map::MemoryMap;
    use crate::crash_info::registers::Registers;
    use crate::crash_info::sig_info::{SiCodes, SigInfo, SignalNames};

    fn sample_memory_map() -> MemoryMap {
        MemoryMap::from_maps_lines(&[
            "55a3f2a00000-55a3f2c00000 r-xp 00000000 fd:01 1234567  /usr/bin/myapp".to_string(),
            "55a3f2e00000-55a3f2f00000 rw-p 00200000 fd:01 1234567  /usr/bin/myapp".to_string(),
            "01000000-02000000 rw-p 00000000 00:00 0        [heap]".to_string(),
            "7f8a12000000-7f8a12200000 r--p 00000000 fd:01 2345678  /usr/lib/libc.so.6".to_string(),
            "7ffc89a00000-7ffc89c00000 rw-p 00000000 00:00 0        [stack]".to_string(),
        ])
    }

    fn make_sig_info(signo: SignalNames, code: SiCodes, addr: Option<&str>) -> SigInfo {
        SigInfo {
            si_addr: addr.map(|s| s.to_string()),
            si_code: 0,
            si_code_human_readable: code,
            si_signo: 0,
            si_signo_human_readable: signo,
        }
    }

    fn make_registers(rip: u64, rsp: u64, rax: u64) -> Registers {
        let mut general = std::collections::HashMap::new();
        general.insert("rip".to_string(), rip);
        general.insert("rsp".to_string(), rsp);
        general.insert("rbp".to_string(), rsp + 0x10);
        general.insert("rax".to_string(), rax);
        Registers {
            ip: Some(rip),
            sp: Some(rsp),
            fp: Some(rsp + 0x10),
            general,
        }
    }

    #[test]
    fn test_null_pointer_dereference_at_zero() {
        let sig = make_sig_info(
            SignalNames::SIGSEGV,
            SiCodes::SEGV_MAPERR,
            Some("0x0000000000000000"),
        );
        let map = sample_memory_map();
        let regs = make_registers(0x55a3f2b00000, 0x7ffc89b00000, 0);

        let diag = diagnose(&sig, &regs, &map);
        assert_eq!(diag.category, DiagnosisCategory::NullPointerDereference);
        assert!(diag.summary.contains("Null pointer dereference"));
        assert!(diag.details.contains("direct null dereference"));
    }

    #[test]
    fn test_null_pointer_dereference_with_offset() {
        let sig = make_sig_info(
            SignalNames::SIGSEGV,
            SiCodes::SEGV_MAPERR,
            Some("0x0000000000000018"),
        );
        let map = sample_memory_map();
        let regs = make_registers(0x55a3f2b00000, 0x7ffc89b00000, 0);

        let diag = diagnose(&sig, &regs, &map);
        assert_eq!(diag.category, DiagnosisCategory::NullPointerDereference);
        assert!(diag.summary.contains("field offset 0x18"));
    }

    #[test]
    fn test_stack_overflow() {
        // Fault address just below stack start
        let sig = make_sig_info(
            SignalNames::SIGSEGV,
            SiCodes::SEGV_MAPERR,
            Some("0x00007ffc899ffff0"),
        );
        let map = sample_memory_map();
        let regs = make_registers(0x55a3f2b00000, 0x7ffc899fffd0, 0);

        let diag = diagnose(&sig, &regs, &map);
        assert_eq!(diag.category, DiagnosisCategory::StackOverflow);
        assert!(diag.summary.contains("Stack overflow"));
    }

    #[test]
    fn test_use_after_free() {
        // Address in heap neighborhood but unmapped
        let sig = make_sig_info(
            SignalNames::SIGSEGV,
            SiCodes::SEGV_MAPERR,
            Some("0x0000000001500000"),
        );
        let map = sample_memory_map();
        let regs = make_registers(0x55a3f2b00000, 0x7ffc89b00000, 0x1500000);

        let diag = diagnose(&sig, &regs, &map);
        assert_eq!(diag.category, DiagnosisCategory::UseAfterFree);
    }

    #[test]
    fn test_wild_pointer() {
        // Address far from anything
        let sig = make_sig_info(
            SignalNames::SIGSEGV,
            SiCodes::SEGV_MAPERR,
            Some("0x0000deadbeef0000"),
        );
        let map = sample_memory_map();
        let regs = make_registers(0x55a3f2b00000, 0x7ffc89b00000, 0);

        let diag = diagnose(&sig, &regs, &map);
        assert_eq!(diag.category, DiagnosisCategory::WildPointer);
    }

    #[test]
    fn test_write_to_readonly() {
        // Address in r--p region of libc
        let sig = make_sig_info(
            SignalNames::SIGSEGV,
            SiCodes::SEGV_ACCERR,
            Some("0x00007f8a12100000"),
        );
        let map = sample_memory_map();
        let regs = make_registers(0x55a3f2b00000, 0x7ffc89b00000, 0);

        let diag = diagnose(&sig, &regs, &map);
        assert_eq!(diag.category, DiagnosisCategory::WriteToReadOnly);
        assert!(diag.details.contains("libc.so.6"));
    }

    #[test]
    fn test_sigabrt() {
        let sig = make_sig_info(SignalNames::SIGABRT, SiCodes::SI_TKILL, None);
        let map = sample_memory_map();
        let regs = make_registers(0x55a3f2b00000, 0x7ffc89b00000, 0);

        let diag = diagnose(&sig, &regs, &map);
        assert_eq!(diag.category, DiagnosisCategory::IntentionalAbort);
    }

    #[test]
    fn test_sigill_in_executable_region() {
        let sig = make_sig_info(SignalNames::SIGILL, SiCodes::UNKNOWN, None);
        let map = sample_memory_map();
        // IP in executable /usr/bin/myapp mapping
        let regs = make_registers(0x55a3f2b00000, 0x7ffc89b00000, 0);

        let diag = diagnose(&sig, &regs, &map);
        assert_eq!(diag.category, DiagnosisCategory::IllegalInstruction);
        assert!(diag.details.contains("executable region"));
    }

    #[test]
    fn test_sigill_in_non_executable_region() {
        let sig = make_sig_info(SignalNames::SIGILL, SiCodes::UNKNOWN, None);
        let map = sample_memory_map();
        // IP in rw-p data section
        let regs = make_registers(0x55a3f2e50000, 0x7ffc89b00000, 0);

        let diag = diagnose(&sig, &regs, &map);
        assert_eq!(diag.category, DiagnosisCategory::ExecuteNonExecutable);
    }

    #[test]
    fn test_sigbus_alignment() {
        let sig = make_sig_info(
            SignalNames::SIGBUS,
            SiCodes::BUS_ADRALN,
            Some("0x0000000012345679"),
        );
        let map = sample_memory_map();
        let regs = make_registers(0x55a3f2b00000, 0x7ffc89b00000, 0);

        let diag = diagnose(&sig, &regs, &map);
        assert_eq!(diag.category, DiagnosisCategory::MisalignedAccess);
    }

    #[test]
    fn test_null_registers_populated() {
        let sig = make_sig_info(
            SignalNames::SIGSEGV,
            SiCodes::SEGV_MAPERR,
            Some("0x0000000000000018"),
        );
        let map = sample_memory_map();
        let regs = make_registers(0x55a3f2b00000, 0x7ffc89b00000, 0);

        let diag = diagnose(&sig, &regs, &map);
        assert!(diag.null_registers.contains(&"rax".to_string()));
    }

    #[test]
    fn test_stack_pointer_valid() {
        let sig = make_sig_info(SignalNames::SIGABRT, SiCodes::SI_TKILL, None);
        let map = sample_memory_map();
        // SP within stack region
        let regs = make_registers(0x55a3f2b00000, 0x7ffc89b00000, 0);

        let diag = diagnose(&sig, &regs, &map);
        assert_eq!(diag.stack_pointer_valid, Some(true));
    }

    #[test]
    fn test_stack_pointer_invalid() {
        let sig = make_sig_info(SignalNames::SIGABRT, SiCodes::SI_TKILL, None);
        let map = sample_memory_map();
        // SP outside any stack region
        let regs = make_registers(0x55a3f2b00000, 0x1234, 0);

        let diag = diagnose(&sig, &regs, &map);
        assert_eq!(diag.stack_pointer_valid, Some(false));
    }

    #[test]
    fn test_crash_location_populated() {
        let sig = make_sig_info(SignalNames::SIGABRT, SiCodes::SI_TKILL, None);
        let map = sample_memory_map();
        let regs = make_registers(0x55a3f2b00000, 0x7ffc89b00000, 0);

        let diag = diagnose(&sig, &regs, &map);
        let loc = diag.crash_location.unwrap();
        assert_eq!(loc.path.as_deref(), Some("/usr/bin/myapp"));
        assert_eq!(loc.permissions, "r-xp");
    }

    #[test]
    fn test_parse_hex_addr() {
        assert_eq!(parse_hex_addr("0x1234"), Some(0x1234));
        assert_eq!(parse_hex_addr("0X1234"), Some(0x1234));
        assert_eq!(parse_hex_addr("1234"), Some(0x1234));
        assert_eq!(parse_hex_addr("0x000055a3f2c01234"), Some(0x55a3f2c01234));
        assert_eq!(parse_hex_addr("not_hex"), None);
    }
}
