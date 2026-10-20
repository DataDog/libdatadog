// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// Bin crates can't meaningfully expose a public API: `pub` items inside the
// bench modules would trip `unreachable_pub` (and `pub(crate)` trips
// `clippy::redundant_pub_crate`), so opt this bench out.
#![allow(unreachable_pub)]

extern crate alloc;

use criterion::criterion_main;

mod benchmarks;

criterion_main!(
    benchmarks::credit_cards_bench::benches,
    benchmarks::redis_obfuscation_bench::benches,
    benchmarks::replace_trace_tags_bench::benches,
    benchmarks::sql_obfuscation_bench::benches,
    benchmarks::ip_address_bench::benches,
);
