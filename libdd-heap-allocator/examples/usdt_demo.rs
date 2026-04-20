// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Sample app exercising `libdd-heap-allocator` as the global allocator.
//!
//! Install `SampledAllocator<System>` globally, then loop producing
//! allocations (strings joined into a single buffer) so a tracer attached
//! to the `ddheap:alloc` USDT probe sees samples fire periodically.
//!
//! Run (Linux, inside the crate's Lima VM):
//! ```
//! cargo run --example usdt_demo -p libdd-heap-allocator
//! ```
//! and in another shell, attach a tracer, e.g.
//! ```
//! sudo bpftrace -p <pid> -e 'usdt:*:ddheap:alloc { printf("alloc %p %d %d\n", arg0, arg1, arg2); }'
//! ```

use libdd_heap_allocator::SampledAllocator;
use std::alloc::System;
use std::thread::sleep;
use std::time::Duration;

#[global_allocator]
static ALLOC: SampledAllocator<System> = SampledAllocator::<System>::DEFAULT;

fn main() {
    println!(
        "pid={}; attach a tracer on 'usdt:*:ddheap:alloc'",
        std::process::id()
    );

    let mut i: u64 = 0;
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
        sleep(Duration::from_secs(1));
    }
}
