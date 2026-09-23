// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! End-to-end test for the `thread-exit-autoclean` feature (see the "Context records lifecycle"
//! section of the README): a context that is still attached when a thread exits must be freed by
//! the TLS-based autocleaner instead of leaking.
//!
//! This test runs as its own binary (`harness = false` in `Cargo.toml`), which guarantees:
//!
//! - a dedicated process, so the counting global allocator below only sees allocations from this
//!   test and from the standard library — not from unrelated unit or integration tests running in
//!   the same process;
//! - a minimal thread population (the main thread plus the single worker thread spawned below),
//!   which keeps the allocation counting deterministic.
//!
//! Since the interesting effect (the record being freed) happens in TLS destructors, i.e. after the
//! worker's body has returned but before `join()` returns, we snapshot the counters at the very end
//! of the thread body and compare them with the counters observed after `join()`.
//!
//! Since `harness = false`, `main` implements the small protocol expected by `cargo nextest` (see
//! https://nexte.st/docs/design/custom-test-harnesses/): `--list --format terse` support and
//! per-test selection with `--exact`.

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod imp {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use libdd_otel_thread_ctx::linux::ThreadContext;

    static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
    static DEALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

    struct CountingAllocator;

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    /// Returns the `(allocations, deallocations)` counters.
    fn counters() -> (usize, usize) {
        (
            ALLOCATIONS.load(Ordering::Relaxed),
            DEALLOCATIONS.load(Ordering::Relaxed),
        )
    }

    /// Runs a thread that allocates and attaches a context record via `update`, and optionally
    /// detaches it explicitly before the end of the body. Snapshots the allocation counters at
    /// various point in the thread lifecycle to make sure the attached context is cleaned up
    /// properly after the thread exited.
    fn run_scenario(explicit_detach: bool, scenario: &str) {
        let (alloc_before, dealloc_before) = counters();

        let (alloc_in_body, dealloc_in_body) = std::thread::spawn(move || {
            // First call on this thread: allocates the record, attaches it, and installs the
            // autocleaner.
            ThreadContext::update([1u8; 16], [2u8; 8], 0, [3u8; 8], &[]);

            if explicit_detach {
                let detached =
                    ThreadContext::detach().expect("a context must be attached after update");
                drop(detached);
            }

            counters()
        })
        .join()
        .unwrap();

        let (alloc_after, dealloc_after) = counters();

        assert!(
            alloc_in_body > alloc_before,
            "{scenario}: the thread body must have allocated the context record"
        );
        if explicit_detach {
            assert!(
                dealloc_in_body > dealloc_before,
                "{scenario}: explicitly detaching and dropping the context must free the record \
                 inside the thread body"
            );
        } else {
            assert_eq!(
                dealloc_in_body, dealloc_before,
                "{scenario}: the body never detaches, so nothing must be freed inside the body"
            );
            assert!(
                dealloc_after > dealloc_in_body,
                "{scenario}: a context still attached at thread exit must be freed after the \
                 thread body returned (by the autocleaner), but no deallocation was observed"
            );
        }
        assert_eq!(
            alloc_after - alloc_before,
            dealloc_after - dealloc_before,
            "{scenario}: allocations and deallocations must balance once the thread exited: the \
             context record must not leak"
        );
    }

    const AUTOCLEAN_TEST: &str = "autoclean_frees_context_on_thread_exit";
    const EXPLICIT_DETACH_TEST: &str = "explicit_detach_frees_context_inside_thread_body";

    /// Implements the small protocol expected by `cargo nextest` (and plain `cargo test`): list
    /// the tests for `--list`, run all scenarios by default, or only the one selected with
    /// `--exact <name>`.
    pub(crate) fn run() {
        let args: Vec<String> = std::env::args().skip(1).collect();

        // `cargo nextest` (and `cargo test -- --list`) discovery protocol: list the tests, one
        // `<name>: test` line per test. This harness has no ignored tests, so `--ignored` prints
        // nothing.
        if args.iter().any(|arg| arg == "--list") {
            if !args.iter().any(|arg| arg == "--ignored") {
                println!("{AUTOCLEAN_TEST}: test");
                println!("{EXPLICIT_DETACH_TEST}: test");
            }
            return;
        }

        // The TLS slot is accessed through inline assembly (TLSDESC), which Miri doesn't support;
        // the crate's other TLS tests are skipped under Miri for the same reason.
        if cfg!(miri) {
            println!("skipped under Miri: inline-asm TLSDESC access is not supported");
            return;
        }

        // Run all scenarios by default (plain `cargo test` run), or only the one selected with
        // `--exact <name>` by nextest.
        for (name, explicit_detach) in [(AUTOCLEAN_TEST, false), (EXPLICIT_DETACH_TEST, true)] {
            if args.is_empty() || args.iter().any(|arg| arg == name) {
                run_scenario(explicit_detach, name);
            }
        }
    }
}

fn main() {
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    imp::run();
}
