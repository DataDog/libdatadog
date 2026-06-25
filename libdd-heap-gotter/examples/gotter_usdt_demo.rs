// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Sample app exercising `libdd-heap-gotter`: install the GOT overrides
//! at startup, then loop producing allocations through the *unmodified*
//! Rust system allocator. Once `install_heap_overrides()` has run, those
//! `malloc`/`free` calls resolve through the patched GOT entries and
//! flow into `libdd-heap-sampler`, firing `ddheap:alloc` / `ddheap:free`
//! USDTs.
//!
//! Run (Linux, inside the crate's Lima VM, or natively under a container):
//! ```
//! cargo run --example gotter_usdt_demo -p libdd-heap-gotter
//! ```
//! Add `-- --stress` to keep a CPU core busy enough for CPU profiles:
//! ```
//! cargo run --example gotter_usdt_demo -p libdd-heap-gotter -- --stress
//! ```
//! and in another shell, attach a tracer, e.g.
//! ```
//! sudo bpftrace -p <pid> -e 'usdt:*:ddheap:alloc { printf("alloc %p %d %d\n", arg0, arg1, arg2); }'
//! ```
//!
//! The gotter crate is Linux-only; on other targets the example
//! compiles to an empty `main` so clippy/test on non-Linux don't fail
//! with "configured out".

#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
fn main() {
    linux::main();
}

#[cfg(target_os = "linux")]
mod linux {
    use std::hint::black_box;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    fn burn_cpu_for(duration: Duration, mut state: u64) -> u64 {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            // A tiny integer workload that is deterministic, dependency-chained,
            // and opaque to the optimizer. This keeps one CPU core busy without
            // changing the allocation profile this example is meant to exercise.
            state =
                state.wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(17) ^ 0xbf58_476d_1ce4_e5b9;
            black_box(state);
        }
        state
    }

    pub fn main() {
        let stress = std::env::args().any(|arg| arg == "--stress");

        println!(
            "pid={}; pre-install. Attach a tracer on 'usdt:*:ddheap:*'. stress={stress}",
            std::process::id()
        );

        // Make a few allocations before install to demonstrate the pre-patch
        // baseline — no USDTs should fire for these.
        {
            let warmup: Vec<String> = (0..16).map(|i| format!("warmup-{i}")).collect();
            println!("pre-install warmup: {} entries", warmup.len());
        }

        sleep(Duration::from_secs(2));

        // Install GOT overrides. After this, malloc/free/calloc/realloc
        // calls anywhere in the process (including those issued by the Rust
        // System allocator backing `Vec`, `String`, etc.) route through
        // `libdd-heap-sampler`.
        let ok = libdd_heap_gotter::install_heap_overrides();
        println!("install_heap_overrides() -> {ok}");

        let mut i: u64 = 0;
        let mut cpu_state = 0x1234_5678_9abc_def0;
        loop {
            // ~1000 small allocations + one larger join: plenty of alloc
            // pressure to cross the default 512 KiB sampling interval over
            // a handful of iterations.
            let parts: Vec<String> = (0..1000)
                .map(|j| format!("chunk-{i}-{j}-with-some-padding-to-make-it-meaningful"))
                .collect();
            let joined = parts.join(", ");
            println!("[{i}] joined {} bytes", joined.len());
            i = i.wrapping_add(1);

            if stress {
                cpu_state = burn_cpu_for(Duration::from_secs(1), cpu_state ^ i);
            } else {
                sleep(Duration::from_secs(1));
            }
        }
    }
} // mod linux
