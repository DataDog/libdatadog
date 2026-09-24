// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! End-to-end test for the `thread-exit-autoclean` feature forwarded to the FFI crate: a context
//! that is still attached when a thread exits must be freed by the TLS-based autocleaner instead
//! of leaking.
//!
//! This test exercises the C API (`ddog_otel_thread_ctx_*`) rather than the pure-Rust API, because
//! the FFI is what third-language consumers (C, Java, .NET, ...) link against: the autocleaner is
//! compiled into the artifact and frees any context left attached on thread exit, with nothing
//! required on the host side.
//!
//! This test runs as its own binary (`harness = false` in `Cargo.toml`), such that the allocator
//! hook only runs for this process and we have total control over spawned threads, avoiding
//! flakiness and interaction with other tests.
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
    use std::ptr;
    use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

    use libdd_otel_thread_ctx::test_utils::read_tls_context_ptr;
    use libdd_otel_thread_ctx_ffi::{
        ddog_otel_thread_ctx_detach, ddog_otel_thread_ctx_free, ddog_otel_thread_ctx_update,
    };

    /// The address of the attached context record. The allocator watches for a
    /// deallocation of this exact address.
    ///
    /// `null` while no scenario is running.
    static TRACKED_CTX: AtomicPtr<u8> = AtomicPtr::new(ptr::null_mut());

    /// Sets to `true` if the allocator freed a pointer that was equal to `TRACKED_CTX` at some
    /// point.
    static TRACKED_CTX_FREED: AtomicBool = AtomicBool::new(false);

    struct TrackingAllocator;

    unsafe impl GlobalAlloc for TrackingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ctx_ptr: *mut u8, layout: Layout) {
            if ctx_ptr == TRACKED_CTX.load(Ordering::Relaxed) {
                TRACKED_CTX_FREED.store(true, Ordering::Relaxed);
            }
            unsafe { System.dealloc(ctx_ptr, layout) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: TrackingAllocator = TrackingAllocator;

    /// Runs a thread that allocates and attaches a context record via
    /// `ddog_otel_thread_ctx_update`, and optionally detaches it explicitly before the end of the
    /// body. Observes whether the attached record has been freed at various points in the thread
    /// lifecycle to make sure the attached context is cleaned up properly once the thread exited.
    fn run_scenario(explicit_detach: bool, scenario: &'static str) {
        // The two scenarios may share a process (plain `cargo test` runs them in sequence), so
        // the tracking state is reset between runs.
        TRACKED_CTX.store(ptr::null_mut(), Ordering::Relaxed);
        TRACKED_CTX_FREED.store(false, Ordering::Relaxed);

        // We spawn one thread and join it, so atomic accesses are unconditionally totally ordered.
        // We can use a `Relaxed` ordering.
        let freed_in_body = std::thread::spawn(move || {
            // First call on this thread: allocates the record, attaches it, and installs the
            // autocleaner.
            ddog_otel_thread_ctx_update(&[1u8; 16], &[2u8; 8], 0, &[3u8; 8]);

            let record = read_tls_context_ptr().cast_mut().cast::<u8>();
            assert!(
                !record.is_null(),
                "{scenario}: `ddog_otel_thread_ctx_update` must have attached a record to the TLS \
                 slot"
            );
            TRACKED_CTX.store(record, Ordering::Relaxed);

            if explicit_detach {
                let detached =
                    ddog_otel_thread_ctx_detach().expect("a context must be attached after update");
                // Safety: `detached` is a non-null handle obtained from this API and is freed
                // exactly once here.
                unsafe { ddog_otel_thread_ctx_free(detached.as_ptr()) };
            }

            TRACKED_CTX_FREED.load(Ordering::Relaxed)
        })
        .join()
        .unwrap();

        let freed_after_exit = TRACKED_CTX_FREED.load(Ordering::Relaxed);

        if explicit_detach {
            assert!(
                freed_in_body,
                "{scenario}: explicitly detaching and freeing the context must release the record \
                 inside the thread body"
            );
        } else {
            assert!(
                !freed_in_body,
                "{scenario}: the body never detaches, so the record must not be freed inside the \
                 body"
            );
            assert!(
                freed_after_exit,
                "{scenario}: a context still attached at thread exit must be freed after the \
                 thread body returned (by the autocleaner), but the record was never deallocated"
            );
        }
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
