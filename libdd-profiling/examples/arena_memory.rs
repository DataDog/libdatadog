// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Step 0 baseline tool for the Windows lazy-commit arena work.
//!
//! It exercises the `ProfilesDictionary` arenas with the realistic usage shape
//! (one long-lived dictionary, lots of strings -- e.g. the .NET profiler) and
//! prints this process's memory footprint, so the *same* binary can be run on
//! `main` and on a feature branch and the deltas diffed. The headline number on
//! Windows is the commit charge (`PROCESS_MEMORY_COUNTERS_EX.PrivateUsage`),
//! which is where eager `MEM_COMMIT` shows up; on Linux the two builds are
//! expected to be identical (mmap overcommit), so it is reported only for
//! completeness.
//!
//! The arenas use fixed ~1 MiB chunks per shard (16 string + 4 function + 2
//! mapping shards), committed on demand as they fill. For a single dictionary
//! the eager-commit *waste* is therefore bounded to the partially-filled top
//! chunk per shard (~22 MiB worst case), regardless of how many strings are
//! interned. This tool makes that relationship visible: as the unique-string
//! count grows, committed memory should track the interned bytes plus a roughly
//! constant slack.
//!
//! This file deliberately uses only the public `ProfilesDictionary` API plus
//! the `WORDPRESS_STRINGS` corpus, with no internal/stats APIs, so the identical
//! file compiles on both `main` and the branch.
//!
//! Usage:
//!   cargo run --release --example arena_memory
//!   cargo run --release --example arena_memory -- <unique_strings> <checkpoints> <sparse_dicts>
//!
//! Defaults: unique_strings=300000, checkpoints=6, sparse_dicts=0. The
//! sparse-dictionaries scenario (the eager-commit multiplier) only runs when
//! sparse_dicts > 0, since it is an edge case rather than the .NET shape.

use std::any::Any;
use std::error::Error;
use std::fmt::Write as _;

use libdd_profiling::collections::string_table::wordpress_test_data::WORDPRESS_STRINGS;
use libdd_profiling::profiles::datatypes::{Function2, Mapping2, ProfilesDictionary, StringId2};

type DynResult<T> = Result<T, Box<dyn Error>>;

/// A set of named memory metrics in bytes. The labels are platform specific,
/// but the order is stable so two snapshots can be diffed positionally.
struct MemSnapshot {
    metrics: Vec<(&'static str, u64)>,
}

#[cfg(target_os = "linux")]
fn read_memory() -> MemSnapshot {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |key: &str| -> u64 {
        status
            .lines()
            .find(|line| line.starts_with(key))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|kb| kb.parse::<u64>().ok())
            .map(|kb| kb * 1024)
            .unwrap_or(0)
    };
    MemSnapshot {
        metrics: vec![
            ("VmRSS", field("VmRSS:")),
            ("VmHWM (peak RSS)", field("VmHWM:")),
            ("VmSize", field("VmSize:")),
        ],
    }
}

#[cfg(windows)]
fn read_memory() -> MemSnapshot {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: GetProcessMemoryInfo fills the provided buffer; we pass a
    // correctly sized, zero-initialized PROCESS_MEMORY_COUNTERS_EX and its size
    // in `cb`, as the API requires.
    let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    let ok = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        )
    };
    if ok == 0 {
        return MemSnapshot {
            metrics: vec![
                ("PrivateUsage (commit charge)", 0),
                ("PeakWorkingSetSize", 0),
                ("WorkingSetSize", 0),
            ],
        };
    }
    MemSnapshot {
        metrics: vec![
            ("PrivateUsage (commit charge)", counters.PrivateUsage as u64),
            ("PeakWorkingSetSize", counters.PeakWorkingSetSize as u64),
            ("WorkingSetSize", counters.WorkingSetSize as u64),
        ],
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn read_memory() -> MemSnapshot {
    MemSnapshot {
        metrics: vec![("unsupported-platform", 0)],
    }
}

fn human(bytes: u64) -> String {
    format!("{:.2} MiB", bytes as f64 / (1024.0 * 1024.0))
}

fn human_delta(bytes: i128) -> String {
    format!("{:+.2} MiB", bytes as f64 / (1024.0 * 1024.0))
}

fn metric_delta(before: &MemSnapshot, after: &MemSnapshot, idx: usize) -> i128 {
    let b = before.metrics.get(idx).map(|(_, v)| *v).unwrap_or(0);
    let a = after.metrics.get(idx).map(|(_, v)| *v).unwrap_or(0);
    a as i128 - b as i128
}

fn report(label: &str, before: &MemSnapshot, after: &MemSnapshot) {
    println!("== {label} ==");
    for (i, (name, after_val)) in after.metrics.iter().enumerate() {
        let before_val = before.metrics.get(i).map(|(_, v)| *v).unwrap_or(0);
        println!(
            "  {name:<30} before={:>12}  after={:>12}  delta={:>13}",
            human(before_val),
            human(*after_val),
            human_delta(*after_val as i128 - before_val as i128),
        );
    }
    println!();
}

/// Generates a unique, realistic-looking string by reusing a corpus entry (so
/// lengths/prefixes resemble real profiler data: method signatures, file paths)
/// and appending a counter so it is not deduplicated.
fn gen_unique(buf: &mut String, i: usize) {
    let base = WORDPRESS_STRINGS[i % WORDPRESS_STRINGS.len()];
    buf.clear();
    let _ = write!(buf, "{base}#{i}");
}

/// Headline scenario: one dictionary, a growing number of unique strings. Prints
/// the footprint at evenly spaced checkpoints so committed memory can be
/// compared against the interned-byte total (the "used" lower bound). A light
/// stream of derived functions/mappings keeps the 4 function and 2 mapping
/// shards exercised too.
fn scenario_scaling(total: usize, checkpoints: usize) -> DynResult<()> {
    println!("== scenario: single dictionary, scaling to {total} unique strings ==");
    println!(
        "  {:>10}  {:>12}  {:>12}  {:>12}  {:>12}",
        "strings", "interned", "rss/private", "peak", "virt/ws"
    );

    let dict = ProfilesDictionary::try_new()?;
    let start = read_memory();

    let step = (total / checkpoints.max(1)).max(1);
    let mut interned_bytes: u64 = 0;
    let mut buf = String::new();
    // A small ring of recent ids used to synthesize functions/mappings.
    let mut recent = [StringId2::EMPTY; 4];

    for i in 0..total {
        gen_unique(&mut buf, i);
        interned_bytes += buf.len() as u64;
        let id = dict.try_insert_str2(&buf)?;
        recent[i % recent.len()] = id;

        // Touch the function/mapping shards occasionally without dominating.
        if i % 8 == 0 {
            dict.try_insert_function2(Function2 {
                name: recent[0],
                system_name: recent[1],
                file_name: recent[2],
            })?;
            dict.try_insert_mapping2(Mapping2 {
                memory_start: i as u64 * 0x1000,
                memory_limit: i as u64 * 0x1000 + 0x800,
                file_offset: 0,
                filename: recent[3],
                build_id: recent[0],
            })?;
        }

        if (i + 1) % step == 0 || i + 1 == total {
            let now = read_memory();
            println!(
                "  {:>10}  {:>12}  {:>12}  {:>12}  {:>12}",
                i + 1,
                human(interned_bytes),
                human_delta(metric_delta(&start, &now, 0)),
                human_delta(metric_delta(&start, &now, 1)),
                human_delta(metric_delta(&start, &now, 2)),
            );
        }
    }
    println!(
        "  (interned unique-string bytes are the 'used' lower bound; the gap to \
         rss/private is hash-table overhead + bounded per-shard chunk slack)\n"
    );

    // Keep the dictionary alive until after the final snapshot above.
    drop(dict);
    Ok(())
}

/// Builds a single dictionary populated with the given strings plus derived
/// functions/mappings. Used by the edge-case many-dictionaries scenario.
fn build_dict(strings: &[&str]) -> DynResult<ProfilesDictionary> {
    let dict = ProfilesDictionary::try_new()?;
    let mut ids = Vec::with_capacity(strings.len());
    for s in strings {
        ids.push(dict.try_insert_str2(s)?);
    }
    let n = ids.len();
    for (i, &name) in ids.iter().enumerate() {
        dict.try_insert_function2(Function2 {
            name,
            system_name: ids[(i + 1) % n],
            file_name: ids[(i + 2) % n],
        })?;
        dict.try_insert_mapping2(Mapping2 {
            memory_start: (i as u64) * 0x1000,
            memory_limit: (i as u64) * 0x1000 + 0x800,
            file_offset: 0,
            filename: name,
            build_id: ids[(i + 3) % n],
        })?;
    }
    Ok(dict)
}

/// Edge case (NOT the .NET shape): many small dictionaries. Each dictionary
/// eagerly commits ~1 MiB per touched shard regardless of how little it holds,
/// so the footprint scales with the number of dictionaries. Only meaningful if
/// many dictionaries are ever live at once.
fn scenario_many_sparse(sparse_dicts: usize, sparse_strings: usize) -> DynResult<()> {
    let slice = &WORDPRESS_STRINGS[..sparse_strings.min(WORDPRESS_STRINGS.len())];
    let before = read_memory();
    let live: Box<dyn Any> = {
        let mut dicts = Vec::with_capacity(sparse_dicts);
        for _ in 0..sparse_dicts {
            dicts.push(build_dict(slice)?);
        }
        Box::new(dicts)
    };
    let after = read_memory();
    report(
        &format!("EDGE scenario: {sparse_dicts} sparse dictionaries x {sparse_strings} strings"),
        &before,
        &after,
    );
    drop(live);
    Ok(())
}

fn main() -> DynResult<()> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |idx: usize, default: usize| -> usize {
        args.get(idx)
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(default)
    };
    let unique_strings = arg(1, 300_000);
    let checkpoints = arg(2, 6);
    let sparse_dicts = arg(3, 0);

    println!(
        "arena_memory: corpus={} strings | unique_strings={} checkpoints={} | sparse_dicts={}\n",
        WORDPRESS_STRINGS.len(),
        unique_strings,
        checkpoints,
        sparse_dicts,
    );

    let start = read_memory();
    println!("== process-start baseline ==");
    for (name, value) in &start.metrics {
        println!("  {name:<30} {:>12}", human(*value));
    }
    println!();

    scenario_scaling(unique_strings, checkpoints)?;

    if sparse_dicts > 0 {
        scenario_many_sparse(sparse_dicts, 10)?;
    }

    let end = read_memory();
    report("total (process start -> end)", &start, &end);

    Ok(())
}
