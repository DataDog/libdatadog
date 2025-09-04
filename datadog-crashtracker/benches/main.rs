// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{criterion_group, criterion_main, Criterion};

#[cfg(all(unix, feature = "benchmarking"))]
mod receiver_bench;

#[cfg(all(unix, feature = "benchmarking", feature = "collector"))]
mod collector_bench;

#[cfg(all(unix, feature = "benchmarking"))]
fn active_benches(_: &mut Criterion) {
    // receiver_bench::benches();
    #[cfg(feature = "collector")]
    collector_bench::collector_benches();
}

#[cfg(any(windows, not(feature = "benchmarking")))]
fn active_benches(_: &mut Criterion) {
    println!("Benchmarks are disabled.");
}
criterion_group!(benches, active_benches);
criterion_main!(benches);
