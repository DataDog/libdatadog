// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Sample app exercising `libdd-profiling-heap-jemalloc` against a real
//! `jemalloc` global allocator.
//!
//! Install `jemalloc` as the global allocator, call
//! [`libdd_profiling_heap_jemalloc::install`], then loop producing
//! allocations (strings joined into a single buffer) so a tracer attached
//! to the `ddheap:alloc` USDT probe sees samples fire periodically.
//!
//! Run (Linux, inside the crate's Lima VM):
//! ```
//! cargo run --example jemalloc_demo -p libdd-profiling-heap-jemalloc
//! ```
//! and in another shell, attach a tracer, e.g.
//! ```
//! sudo bpftrace -p <pid> -e 'usdt:*:ddheap:alloc { printf("alloc %p %d %d\n", arg0, arg1, arg2); }'
//! ```
//!
//! `libdd_profiling_heap_jemalloc::install` is Linux-only; on other targets
//! the example compiles to an empty `main` so clippy/test on non-Linux
//! don't fail with "configured out".

#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
fn main() {
    linux::main();
}

#[cfg(target_os = "linux")]
mod linux {
    use std::thread::sleep;
    use std::time::Duration;

    #[global_allocator]
    static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

    pub fn main() {
        libdd_profiling_heap_jemalloc::install().unwrap();

        println!(
            "pid={}; attach a tracer on 'usdt:*:ddheap:alloc'",
            std::process::id()
        );

        let mut i: u64 = 0;
        loop {
            // ~1000 small allocations + one larger join: plenty of alloc
            // pressure to cross the 512 KiB sampling interval `install`
            // resets jemalloc to, over a handful of iterations.
            let parts: Vec<String> = (0..1000)
                .map(|j| format!("chunk-{i}-{j}-with-some-padding-to-make-it-meaningful"))
                .collect();
            let joined = parts.join(", ");
            println!("[{i}] joined {} bytes", joined.len());
            i = i.wrapping_add(1);
            sleep(Duration::from_secs(1));
        }
    }
}
