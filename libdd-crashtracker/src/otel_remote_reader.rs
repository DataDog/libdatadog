// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Out-of-process OTel thread context collection.
//!
//! Discovers the `otel_thread_ctx_v1` TLS symbol entirely from outside the crashing process by
//! scanning `/proc/<pid>/maps`, parsing ELF files, and walking the dynamic linker's `link_map`
//! chain. This avoids `dl_iterate_phdr` (which takes the loader lock and can deadlock in a
//! post-fork collector if a background thread held it at crash time).
//!
//! The receiver calls [`collect_remote_otel_contexts`] after receiving the crash report from the
//! collector, while the crashing process is still alive waiting for the receiver to finish.

use crate::crash_info::OtelThreadContext;
use crate::otel_tls_reader::{ProcMemReader, decode_record, read_thread_name};
use crate::ptrace_collector::{
    attach_thread, detach_thread, enumerate_threads, read_thread_pointer,
};
use core::time::Duration;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// TLS symbol info discovered from ELF files on disk.
struct TlsSymbolInfo {
    /// The symbol's `st_value` — byte offset within the module's TLS block.
    symbol_offset: usize,
    /// PT_TLS `p_memsz` from the ELF program headers.
    tls_memsz: usize,
    /// PT_TLS `p_align` from the ELF program headers.
    tls_align: usize,
}

/// Signed byte offset from the thread pointer (TP) to the DTV pointer.
///
/// These are well-known, ABI-stable offsets for each (architecture, libc) combination.
/// See GDB's `get_tls_dtv_addr` for reference values.
fn dtv_pointer_offset() -> Option<isize> {
    #[cfg(all(target_arch = "x86_64", target_env = "gnu"))]
    return Some(8);

    #[cfg(all(target_arch = "aarch64", target_env = "gnu"))]
    return Some(0);

    #[cfg(all(target_arch = "x86_64", target_env = "musl"))]
    return Some(8);

    #[cfg(all(target_arch = "aarch64", target_env = "musl"))]
    return Some(-8);

    #[allow(unreachable_code)]
    None
}

// ── ELF symbol discovery ────────────────────────────────────────────────────

const SYMBOL_NAME: &[u8] = b"otel_thread_ctx_v1";

/// Scan `/proc/<pid>/maps` and the corresponding ELF files to find the
/// `otel_thread_ctx_v1` TLS symbol.
fn discover_tls_symbol(pid: u32) -> Option<TlsSymbolInfo> {
    let maps_data = std::fs::read_to_string(format!("/proc/{pid}/maps")).ok()?;

    // Collect unique library paths from maps (deduplicate since libraries have multiple mappings).
    let mut seen = std::collections::HashSet::new();
    let mut candidates: Vec<(PathBuf, u64)> = Vec::new();

    for line in maps_data.lines() {
        // Format: "addr-addr perms offset dev inode   pathname"
        // Use split_whitespace to handle variable-width columns.
        let mut fields = line.split_whitespace();
        let Some(addr_range) = fields.next() else {
            continue;
        };
        // Skip perms, offset, dev, inode.
        if fields.nth(3).is_none() {
            continue;
        }
        let Some(path_str) = fields.next() else {
            continue;
        };
        if path_str.starts_with('[') {
            continue;
        }
        let path = Path::new(path_str);
        if !path.is_absolute() {
            continue;
        }
        if !seen.insert(path_str.to_string()) {
            continue;
        }
        // Parse the mapping start address.
        let mapping_start = addr_range
            .split('-')
            .next()
            .and_then(|s| u64::from_str_radix(s, 16).ok())
            .unwrap_or(0);

        candidates.push((path.to_path_buf(), mapping_start));
    }

    for (path, _mapping_start) in candidates {
        if let Some(info) = find_tls_symbol_in_elf(&path) {
            return Some(info);
        }
    }

    None
}

/// Parse an ELF file on disk and search its dynamic symbol table for
/// `otel_thread_ctx_v1` (a `STT_TLS` symbol). Returns the symbol info including
/// PT_TLS metadata needed for static TLS fallback on old glibc.
fn find_tls_symbol_in_elf(path: &Path) -> Option<TlsSymbolInfo> {
    let data = std::fs::read(path).ok()?;
    let elf = goblin::elf::Elf::parse(&data).ok()?;

    let mut symbol_offset = None;
    for sym in elf.dynsyms.iter() {
        if sym.st_type() != goblin::elf::sym::STT_TLS {
            continue;
        }
        if sym.st_shndx == goblin::elf::section_header::SHN_UNDEF as usize {
            continue;
        }
        if let Some(name) = elf.dynstrtab.get_at(sym.st_name)
            && name.as_bytes() == SYMBOL_NAME
        {
            symbol_offset = Some(sym.st_value as usize);
            break;
        }
    }
    let symbol_offset = symbol_offset?;

    // Read PT_TLS memsz and align for computing static TLS addresses when the DTV
    // entry is TLS_DTV_UNALLOCATED (glibc 2.17 and older).
    let (tls_memsz, tls_align) = elf
        .program_headers
        .iter()
        .find(|ph| ph.p_type == goblin::elf::program_header::PT_TLS)
        .map(|ph| (ph.p_memsz as usize, ph.p_align as usize))
        .unwrap_or((0, 1));

    Some(TlsSymbolInfo {
        symbol_offset,
        tls_memsz,
        tls_align,
    })
}

// ── DTV probing ─────────────────────────────────────────────────────────────

/// Read a `u64` from a remote process's memory via `/proc/<pid>/mem`.
fn remote_read_u64(mem: &mut ProcMemReader, addr: u64) -> Option<u64> {
    let mut buf = [0u8; 8];
    mem.read_exact(addr, &mut buf).ok()?;
    Some(u64::from_ne_bytes(buf))
}

/// Result of DTV probing: how to resolve TLS addresses for all threads.
enum TlsResolution {
    /// DTV entry was populated; use the standard DTV walk for all threads.
    Dtv { module_id: usize },
    /// DTV entry was `TLS_DTV_UNALLOCATED` (-1); the TLS block is at a fixed offset
    /// from TP (static TLS, common on glibc 2.17). Use TP-relative addressing.
    StaticOffset { tp_to_slot: i64 },
}

/// Discover how to resolve the OTel TLS variable by probing DTV entries on a stopped thread.
///
/// Instead of walking glibc's internal `link_map` chain (whose `l_tls_modid` offset varies
/// across glibc versions), this probes DTV entries 1..N:
///
/// - **Populated entry**: check if `dtv[i] + symbol_offset` → valid record pointer.
/// - **`TLS_DTV_UNALLOCATED` (-1)**: compute the static TLS block address from TP and the PT_TLS
///   segment size, then validate. This is the path glibc 2.17 (CentOS 7) takes.
fn probe_dtv_for_resolution(
    mem: &mut ProcMemReader,
    tp: u64,
    dtv_offset: isize,
    symbol: &TlsSymbolInfo,
) -> Option<TlsResolution> {
    #[cfg(target_env = "gnu")]
    const DTV_ENTRY_SIZE: u64 = 16;
    #[cfg(target_env = "musl")]
    const DTV_ENTRY_SIZE: u64 = 8;

    const MAX_PROBE: usize = 64;

    let dtv_addr = tp.checked_add_signed(dtv_offset as i64)?;
    let dtv_ptr = remote_read_u64(mem, dtv_addr)?;
    if dtv_ptr == 0 {
        return None;
    }

    for modid in 1..=MAX_PROBE {
        let entry_val =
            remote_read_u64(mem, dtv_ptr + (modid as u64) * DTV_ENTRY_SIZE).unwrap_or(0);

        if entry_val == 0 {
            continue;
        }

        let tls_block_base = if entry_val == u64::MAX {
            // TLS_DTV_UNALLOCATED: the DTV entry hasn't been lazily filled, but for
            // static TLS the block is always allocated next to the TCB. Compute its
            // address from TP and the PT_TLS block size.
            compute_static_tls_base(tp, symbol.tls_memsz, symbol.tls_align)?
        } else {
            entry_val
        };

        // Check if this TLS block + symbol_offset → non-null slot → valid record.
        let slot_addr = tls_block_base + symbol.symbol_offset as u64;
        let slot_val = remote_read_u64(mem, slot_addr).unwrap_or(0);
        if slot_val == 0 {
            continue;
        }
        let mut valid_byte = [0u8];
        if mem.read_exact(slot_val + 24, &mut valid_byte).is_err() || valid_byte[0] != 1 {
            continue;
        }

        // Found the right module.
        return if entry_val == u64::MAX {
            // Static TLS: record the TP-relative offset for use on all threads.
            let tp_to_slot = slot_addr as i64 - tp as i64;
            Some(TlsResolution::StaticOffset { tp_to_slot })
        } else {
            Some(TlsResolution::Dtv { module_id: modid })
        };
    }

    None
}

/// Compute the base address of a static TLS block from the thread pointer.
///
/// On glibc 2.17 and older, the DTV entry for static TLS modules is `TLS_DTV_UNALLOCATED`
/// (-1), but the block is always allocated inline with the TCB.
///
/// - **x86_64 (variant II)**: TLS grows downward: `TP - align_up(memsz, align)`
/// - **aarch64 (variant I)**: TLS grows upward: `align_up(TP + 16, align)`
fn compute_static_tls_base(tp: u64, tls_memsz: usize, tls_align: usize) -> Option<u64> {
    if tls_memsz == 0 {
        return None;
    }
    let align = if tls_align == 0 { 1 } else { tls_align };
    let aligned_size = (tls_memsz + align - 1) & !(align - 1);

    #[cfg(target_arch = "x86_64")]
    return Some(tp.wrapping_sub(aligned_size as u64));
    #[cfg(target_arch = "aarch64")]
    return Some({
        // TCB is 16 bytes on aarch64 glibc (2 pointers: dtv + private).
        let after_tcb = tp + 16;
        ((after_tcb as usize + align - 1) & !(align - 1)) as u64
    });
    #[allow(unreachable_code)]
    None
}

// ── Per-thread collection ───────────────────────────────────────────────────

/// Collect OTel thread contexts from a remote process entirely out-of-process.
///
/// # Steps
///
/// 1. Find `otel_thread_ctx_v1` by scanning `/proc/<pid>/maps` and parsing ELF files.
/// 2. Enumerate threads, ptrace-attach one, open `/proc/<pid>/mem`, then probe DTV entries to
///    discover the TLS module ID (avoids glibc-version-dependent `link_map` offsets).
/// 3. For each thread: ptrace-attach, read TP, resolve TLS via DTV, read record, detach.
///
/// Returns the collected contexts. Failures in individual threads are silently skipped.
pub(crate) fn collect_remote_otel_contexts(
    pid: u32,
    crashing_tid: Option<u32>,
    max_threads: usize,
    budget: Duration,
) -> Vec<OtelThreadContext> {
    let deadline = Instant::now() + budget;
    let mut contexts = Vec::new();

    // Step 1: Discover the TLS symbol from ELF files on disk (no process memory access needed).
    let Some(symbol_info) = discover_tls_symbol(pid) else {
        return contexts;
    };

    let Some(dtv_off) = dtv_pointer_offset() else {
        return contexts;
    };

    // Step 2: Enumerate threads.
    let Ok(tids) = enumerate_threads(pid as i32) else {
        return contexts;
    };

    // Step 3: Ptrace-attach to one thread, then open /proc/<pid>/mem.
    // Some kernels require an active ptrace relationship before allowing /proc/<pid>/mem access.
    let stop_timeout = core::time::Duration::from_millis(100);
    let crashing_tid_i32 = crashing_tid.unwrap_or(0) as i32;

    let first_tid = if crashing_tid_i32 != 0 && tids.contains(&crashing_tid_i32) {
        crashing_tid_i32
    } else if let Some(&tid) = tids.first() {
        tid
    } else {
        return contexts;
    };

    let stop_deadline = (Instant::now() + stop_timeout).min(deadline);
    if attach_thread(first_tid, stop_deadline).is_err() {
        return contexts;
    }

    let Ok(mut mem) = ProcMemReader::open(pid) else {
        let _ = detach_thread(first_tid);
        return contexts;
    };

    // Step 4: Discover how to resolve the OTel TLS variable by probing DTV entries.
    // This handles both modern glibc (populated DTV entries) and glibc 2.17 / CentOS 7
    // (TLS_DTV_UNALLOCATED entries that require static TLS computation).
    let first_tp = match read_thread_pointer(first_tid) {
        Ok(tp) => tp,
        Err(_) => {
            let _ = detach_thread(first_tid);
            return contexts;
        }
    };

    let Some(resolution) = probe_dtv_for_resolution(&mut mem, first_tp, dtv_off, &symbol_info)
    else {
        let _ = detach_thread(first_tid);
        return contexts;
    };

    /// Resolve the TLS slot address for a given thread pointer and resolution strategy.
    fn resolve_slot(
        mem: &mut ProcMemReader,
        tp: u64,
        dtv_off: isize,
        resolution: &TlsResolution,
        symbol: &TlsSymbolInfo,
    ) -> Option<u64> {
        match *resolution {
            TlsResolution::Dtv { module_id } => mem
                .resolve_tls_addr(tp, dtv_off, module_id, symbol.symbol_offset)
                .ok(),
            TlsResolution::StaticOffset { tp_to_slot } => {
                Some((tp as i64).wrapping_add(tp_to_slot) as u64)
            }
        }
    }

    // Read the first thread's TLS record (it's already attached and TP is known).
    let mut record_buf = [0u8; 640];
    let first_result = (|| -> Option<OtelThreadContext> {
        let slot_addr = resolve_slot(&mut mem, first_tp, dtv_off, &resolution, &symbol_info)?;
        let record_ptr = mem.read_tls_slot(slot_addr).ok()?;
        if record_ptr == 0 {
            return None;
        }
        let record_len = mem.read_record(record_ptr, &mut record_buf).ok()??;
        let mut ctx = decode_record(first_tid, &record_buf[..record_len])?;
        ctx.thread_name = read_thread_name(pid, first_tid);
        Some(ctx)
    })();
    let _ = detach_thread(first_tid);
    if let Some(ctx) = first_result {
        contexts.push(ctx);
    }

    // Step 6: Read remaining threads.
    let mut try_read_otel = |tid: libc::pid_t| -> Option<OtelThreadContext> {
        let stop_deadline = (Instant::now() + stop_timeout).min(deadline);
        attach_thread(tid, stop_deadline).ok()?;

        let result = (|| -> Option<OtelThreadContext> {
            let tp = read_thread_pointer(tid).ok()?;

            let slot_addr = resolve_slot(&mut mem, tp, dtv_off, &resolution, &symbol_info)?;

            let record_ptr = mem.read_tls_slot(slot_addr).ok()?;
            if record_ptr == 0 {
                return None;
            }
            let record_len = mem.read_record(record_ptr, &mut record_buf).ok()??;

            let mut ctx = decode_record(tid, &record_buf[..record_len])?;
            ctx.thread_name = read_thread_name(pid, tid);
            Some(ctx)
        })();

        let _ = detach_thread(tid);
        result
    };

    let mut processed = 0;
    for tid in &tids {
        // Skip the first thread (already processed above).
        if *tid == first_tid {
            continue;
        }
        if Instant::now() >= deadline || processed >= max_threads {
            break;
        }
        if let Some(ctx) = try_read_otel(*tid) {
            contexts.push(ctx);
            processed += 1;
        }
    }

    contexts
}
