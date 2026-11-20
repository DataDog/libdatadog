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
//! Contract and toy-scenario tests are strict. Debugging an injected failure: run a worker under
//! a debugger with a breakpoint on `alloc_chaos_injection_hook`.

mod allocator;
mod contract;
mod controller;
mod scenarios;

use allocator::ScopedFailingAllocator;
use controller::{Mode, Outcome};
use std::alloc::System;

#[global_allocator]
static ALLOCATOR: ScopedFailingAllocator<System> = ScopedFailingAllocator(System);

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
];

fn main() {
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
