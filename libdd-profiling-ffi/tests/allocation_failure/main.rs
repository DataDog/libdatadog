// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Experimental allocation-failure harness: finds the paths where a failed allocation (the
//! allocator returns null) crashes instead of returning an out-of-memory error.
//!
//! This test runs as its own binary (`harness = false` in `Cargo.toml`) so that
//! `ScopedFailingAllocator` is the global allocator of this process only. `main` implements the
//! small protocol expected by `cargo nextest` (see
//! https://nexte.st/docs/design/custom-test-harnesses/): `--list --format terse` support and
//! per-test selection with `--exact`. Started with `__worker`, the binary is a worker process of
//! the controller (see `controller.rs`).
//!
//! Contract and toy-scenario tests are strict. Discovery scenarios record each outcome, including
//! a crash, and fail only on a harness error. For those, the worker writes the return addresses
//! of its first injected failure, and the controller symbolizes them to find the owned call site
//! (`ALLOC_CHAOS_DEBUGGER=gdb|lldb` uses a debugger instead). Workers run in parallel
//! (`ALLOC_CHAOS_JOBS`).

mod allocator;
mod arena;
mod contract;
mod controller;
mod lifecycle;
mod scenarios;

// The lifecycle scenarios call the C symbols of this crate: link its library.
use datadog_profiling_ffi as _;

use allocator::{Domain, ScopedFailingAllocator};
use controller::{Mode, Outcome, Report};
use std::alloc::System;

#[global_allocator]
static ALLOCATOR: ScopedFailingAllocator<System> = ScopedFailingAllocator(System);

/// Discovery scenario: every ordinal in both modes is recorded. A crash is a finding.
/// A nondeterministic trace is a harness error.
fn discover(name: &'static str) {
    let _ = load_discovery(name, false);
}

/// Same as [`discover`] for a call whose allocations depend on I/O timing (a request to a
/// local endpoint). An unstable baseline and nondeterministic runs are reported, not errors.
fn discover_io(name: &'static str) {
    let _ = load_discovery(name, true);
}

/// Same as [`discover`], then checks which allocation domains the baseline contains.
fn discover_arena(name: &'static str, required: &[Domain], absent: &[Domain]) -> Report {
    let report = load_discovery(name, false);
    let present: Vec<Domain> = report.baseline.iter().map(|e| e.domain).collect();
    for domain in required {
        assert!(
            present.contains(domain),
            "{name}: baseline {present:?} is missing {domain:?}"
        );
    }
    for domain in absent {
        assert!(
            !present.contains(domain),
            "{name}: baseline {present:?} unexpectedly contains {domain:?}"
        );
    }
    report
}

/// Arena domains a `libdd-profiling` collection must show, when its arenas are observed.
fn profiling_arena_domains() -> &'static [Domain] {
    if arena::PROFILING_ARENAS_OBSERVED {
        &[Domain::ArenaRequest, Domain::ArenaBacking]
    } else {
        &[]
    }
}

/// The request domain alone: a call whose items fit in the chunks the profile already has.
fn profiling_arena_request() -> &'static [Domain] {
    if arena::PROFILING_ARENAS_OBSERVED {
        &[Domain::ArenaRequest]
    } else {
        &[]
    }
}

fn load_discovery(name: &'static str, io_bound: bool) -> Report {
    let report = controller::enumerate_with_stacks(name, io_bound)
        .unwrap_or_else(|e| panic!("harness error: {e}"));
    let nondeterministic: Vec<_> = report
        .runs
        .iter()
        .filter(|finding| finding.run.outcome == Outcome::Nondeterministic)
        .map(|finding| {
            format!(
                "{} #{} {}",
                finding.mode.as_str(),
                finding.ordinal,
                finding.run.detail
            )
        })
        .collect();
    assert!(
        io_bound || nondeterministic.is_empty(),
        "{name}: nondeterministic trace: {nondeterministic:?}"
    );
    let missing: Vec<String> = report
        .runs
        .iter()
        .filter(|finding| {
            let needs_stack = !matches!(
                finding.run.outcome,
                Outcome::OomError | Outcome::Nondeterministic | Outcome::Timeout
            );
            needs_stack
                && finding.allocation_stack.is_some()
                && !finding
                    .call_site
                    .as_deref()
                    .is_some_and(|site| site.contains("libdd"))
        })
        .map(|finding| format!("{} #{}", finding.mode.as_str(), finding.ordinal))
        .collect();
    assert!(
        missing.is_empty(),
        "{name}: no owned call site for {missing:?}"
    );
    report
}

/// Enumerates a toy scenario and checks every run has the `expected` outcome.
fn toy(name: &'static str, expected: Outcome) {
    let report = controller::enumerate(name).unwrap_or_else(|e| panic!("harness error: {e}"));
    assert!(!report.baseline.is_empty(), "{name}: no allocation");
    for mode in [Mode::FailOnce, Mode::FailFrom] {
        let outcomes = report.outcomes(mode);
        assert_eq!(outcomes.len(), report.baseline.len());
        assert!(
            outcomes.iter().all(|&o| o == expected),
            "{name}: expected {} in {mode:?}, got {outcomes:?}",
            expected.as_str()
        );
    }
}

const TESTS: &[(&str, fn())] = &[
    ("contract::pass_through", contract::pass_through),
    ("contract::ordinal_counting", contract::ordinal_counting),
    ("contract::fail_once", contract::fail_once),
    ("contract::fail_from", contract::fail_from),
    (
        "contract::realloc_preserves_block",
        contract::realloc_preserves_block,
    ),
    ("contract::nested_scopes", contract::nested_scopes),
    ("contract::thread_isolation", contract::thread_isolation),
    ("contract::prefix_divergence", contract::prefix_divergence),
    ("toy::box_new", || {
        toy(scenarios::TOY_BOX_NEW, Outcome::Abort)
    }),
    ("toy::vec_push", || {
        toy(scenarios::TOY_VEC_PUSH, Outcome::Abort)
    }),
    ("toy::vec_try_reserve", || {
        toy(scenarios::TOY_VEC_TRY_RESERVE, Outcome::OomError)
    }),
    ("toy::string_format", || {
        toy(scenarios::TOY_STRING_FORMAT, Outcome::Abort)
    }),
    ("toy::contract_violation", || {
        toy(
            scenarios::TOY_CONTRACT_VIOLATION,
            Outcome::ContractViolation,
        )
    }),
    ("scenario::tag_push_empty", || {
        discover(scenarios::TAG_PUSH_EMPTY)
    }),
    ("scenario::tag_push_at_capacity", || {
        discover(scenarios::TAG_PUSH_AT_CAPACITY)
    }),
    ("scenario::tag_push_long", || {
        discover(scenarios::TAG_PUSH_LONG)
    }),
    ("scenario::arena_first", || {
        discover_arena(
            scenarios::ARENA_FIRST,
            &[Domain::ArenaRequest, Domain::ArenaBacking],
            &[],
        );
    }),
    ("scenario::arena_spare", || {
        discover_arena(
            scenarios::ARENA_SPARE,
            &[Domain::ArenaRequest],
            &[Domain::ArenaBacking],
        );
    }),
    ("scenario::arena_exhausted", || {
        discover_arena(
            scenarios::ARENA_EXHAUSTED,
            &[Domain::ArenaRequest],
            &[Domain::ArenaBacking],
        );
    }),
    ("scenario::arena_grow", || {
        discover_arena(
            scenarios::ARENA_GROW,
            &[Domain::ArenaRequest, Domain::ArenaBacking],
            &[],
        );
    }),
    ("scenario::arena_oversized", || {
        let report = discover_arena(
            scenarios::ARENA_OVERSIZED,
            &[Domain::ArenaRequest, Domain::ArenaBacking],
            &[],
        );
        let hint = scenarios::ARENA_CHUNK_HINT as u64;
        assert!(
            report
                .baseline
                .iter()
                .any(|event| { event.domain == Domain::ArenaBacking && event.size > hint }),
            "oversized item did not grow a backing chunk past the hint"
        );
    }),
    ("scenario::arena_string_table", || {
        discover_arena(
            scenarios::ARENA_STRING_TABLE,
            profiling_arena_domains(),
            &[],
        );
    }),
    ("scenario::arena_set", || {
        discover_arena(scenarios::ARENA_SET, profiling_arena_domains(), &[]);
    }),
    ("scenario::arena_slice_set", || {
        discover_arena(scenarios::ARENA_SLICE_SET, profiling_arena_domains(), &[]);
    }),
    ("lifecycle::profile_new", || {
        discover_arena(lifecycle::PROFILE_NEW, profiling_arena_domains(), &[]);
    }),
    ("lifecycle::profile_add_first", || {
        discover_arena(lifecycle::PROFILE_ADD_FIRST, profiling_arena_request(), &[]);
    }),
    ("lifecycle::profile_add_interned", || {
        discover(lifecycle::PROFILE_ADD_INTERNED)
    }),
    ("lifecycle::profile_add_new_strings", || {
        discover_arena(
            lifecycle::PROFILE_ADD_NEW_STRINGS,
            profiling_arena_request(),
            &[],
        );
    }),
    ("lifecycle::profile_add_timestamped", || {
        discover(lifecycle::PROFILE_ADD_TIMESTAMPED)
    }),
    ("lifecycle::profile_add_growth", || {
        discover_arena(
            lifecycle::PROFILE_ADD_GROWTH,
            profiling_arena_request(),
            &[],
        );
    }),
    ("lifecycle::profile_serialize_empty", || {
        discover_arena(
            lifecycle::PROFILE_SERIALIZE_EMPTY,
            profiling_arena_domains(),
            &[],
        );
    }),
    ("lifecycle::profile_serialize_populated", || {
        discover_arena(
            lifecycle::PROFILE_SERIALIZE_POPULATED,
            profiling_arena_domains(),
            &[],
        );
    }),
    ("lifecycle::profile_serialize_endpoints", || {
        discover_arena(
            lifecycle::PROFILE_SERIALIZE_ENDPOINTS,
            profiling_arena_domains(),
            &[],
        );
    }),
    ("lifecycle::profile_serialize_upscaling", || {
        discover_arena(
            lifecycle::PROFILE_SERIALIZE_UPSCALING,
            profiling_arena_domains(),
            &[],
        );
    }),
    ("lifecycle::profile_serialize_large", || {
        discover_arena(
            lifecycle::PROFILE_SERIALIZE_LARGE,
            profiling_arena_domains(),
            &[],
        );
    }),
    ("lifecycle::exporter_new", || {
        discover(lifecycle::EXPORTER_NEW)
    }),
    ("lifecycle::exporter_send_cold", || {
        discover_io(lifecycle::EXPORTER_SEND_COLD)
    }),
    ("lifecycle::exporter_send_warm", || {
        discover_io(lifecycle::EXPORTER_SEND_WARM)
    }),
];

fn main() {
    arena::register_observer();
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.first().map(String::as_str) == Some(controller::WORKER_ARG) {
        controller::worker_main(&args[1..]);
    }

    if args.iter().any(|arg| arg == "--list") {
        if !args.iter().any(|arg| arg == "--ignored") {
            for (name, _) in TESTS {
                println!("{name}: test");
            }
        }
        return;
    }

    // Miri cannot start worker processes.
    if cfg!(miri) {
        println!("skipped under Miri: the harness starts worker processes");
        return;
    }

    let exact = args.iter().any(|a| a == "--exact");
    let filters: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(String::as_str)
        .collect();
    let mut failed = 0;
    for (name, test) in TESTS {
        let selected = filters.is_empty()
            || filters
                .iter()
                .any(|f| if exact { f == name } else { name.contains(f) });
        if !selected {
            continue;
        }
        match std::panic::catch_unwind(test) {
            Ok(()) => println!("test {name} ... ok"),
            Err(_) => {
                println!("test {name} ... FAILED");
                failed += 1;
            }
        }
    }
    if failed > 0 {
        std::process::exit(101);
    }
}
