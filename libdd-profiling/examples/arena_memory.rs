// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Step 0 baseline tool for the Windows lazy-commit arena work.
//!
//! It exercises the `ProfilesDictionary` arenas with a realistic workload and
//! prints this process's memory footprint, so the *same* binary can be run on
//! `main` and on a feature branch and the deltas diffed. The headline number on
//! Windows is the commit charge (`PROCESS_MEMORY_COUNTERS_EX.PrivateUsage`),
//! which is where eager `MEM_COMMIT` shows up; on Linux the two builds are
//! expected to be identical (mmap overcommit), so it is reported only for
//! completeness.
//!
//! This file deliberately uses only the public `ProfilesDictionary` API plus
//! the `WORDPRESS_STRINGS` corpus, with no internal/stats APIs, so the identical
//! file compiles on both `main` and the branch.
//!
//! Usage:
//!   cargo run --release --example arena_memory
//!   cargo run --release --example arena_memory -- <sparse_dicts> <sparse_strings> <filled_dicts>
//!
//! Defaults: sparse_dicts=200, sparse_strings=10, filled_dicts=20. Pass 0 for
//! sparse_dicts or filled_dicts to skip that scenario.

use std::any::Any;
use std::error::Error;

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

fn report(label: &str, before: &MemSnapshot, after: &MemSnapshot) {
    println!("== {label} ==");
    for (i, (name, after_val)) in after.metrics.iter().enumerate() {
        let before_val = before.metrics.get(i).map(|(_, v)| *v).unwrap_or(0);
        let delta = *after_val as i128 - before_val as i128;
        println!(
            "  {name:<30} before={:>12}  after={:>12}  delta={:>13}",
            human(before_val),
            human(*after_val),
            human_delta(delta),
        );
    }
    println!();
}

fn intern_all(dict: &ProfilesDictionary, strings: &[&str]) -> DynResult<Vec<StringId2>> {
    let mut ids = Vec::with_capacity(strings.len());
    for s in strings {
        ids.push(dict.try_insert_str2(s)?);
    }
    Ok(ids)
}

/// Derives functions and mappings from interned string ids so the workload
/// also exercises the 4-shard function set and 2-shard mapping set, not just
/// the 16-shard string set.
fn add_funcs_and_mappings(dict: &ProfilesDictionary, ids: &[StringId2]) -> DynResult<()> {
    let n = ids.len();
    if n == 0 {
        return Ok(());
    }
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
    Ok(())
}

/// Builds a single dictionary populated with the given strings plus derived
/// functions/mappings.
fn build_dict(strings: &[&str]) -> DynResult<ProfilesDictionary> {
    let dict = ProfilesDictionary::try_new()?;
    let ids = intern_all(&dict, strings)?;
    add_funcs_and_mappings(&dict, &ids)?;
    Ok(dict)
}

/// Measures the memory delta across building a workload. The live data is held
/// until after the snapshot is taken, then dropped before returning.
fn run_scenario<F>(label: &str, build: F) -> DynResult<()>
where
    F: FnOnce() -> DynResult<Box<dyn Any>>,
{
    let before = read_memory();
    let live = build()?;
    let after = read_memory();
    report(label, &before, &after);
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
    let sparse_dicts = arg(1, 200);
    let sparse_strings = arg(2, 10).min(WORDPRESS_STRINGS.len());
    let filled_dicts = arg(3, 20);

    println!(
        "arena_memory: corpus={} strings | sparse={}x{} | filled={}\n",
        WORDPRESS_STRINGS.len(),
        sparse_dicts,
        sparse_strings,
        filled_dicts,
    );

    let start = read_memory();
    println!("== process-start baseline ==");
    for (name, value) in &start.metrics {
        println!("  {name:<30} {:>12}", human(*value));
    }
    println!();

    run_scenario("scenario 1: single dictionary, full WordPress corpus", || {
        Ok(Box::new(build_dict(&WORDPRESS_STRINGS)?) as Box<dyn Any>)
    })?;

    if sparse_dicts > 0 {
        let slice = &WORDPRESS_STRINGS[..sparse_strings];
        run_scenario(
            &format!("scenario 2: {sparse_dicts} sparse dictionaries x {sparse_strings} strings"),
            || {
                let mut dicts = Vec::with_capacity(sparse_dicts);
                for _ in 0..sparse_dicts {
                    dicts.push(build_dict(slice)?);
                }
                Ok(Box::new(dicts) as Box<dyn Any>)
            },
        )?;
    }

    if filled_dicts > 0 {
        run_scenario(
            &format!("scenario 3: {filled_dicts} fully-filled dictionaries"),
            || {
                let mut dicts = Vec::with_capacity(filled_dicts);
                for _ in 0..filled_dicts {
                    dicts.push(build_dict(&WORDPRESS_STRINGS)?);
                }
                Ok(Box::new(dicts) as Box<dyn Any>)
            },
        )?;
    }

    let end = read_memory();
    report("total (process start -> end)", &start, &end);

    Ok(())
}
