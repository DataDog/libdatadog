// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{criterion_main, criterion_group, Criterion};

#[cfg(feature = "benchmarking")]
mod receiver_bench;

#[cfg(feature = "benchmarking")]
fn active_benches(c: &mut Criterion) {
    receiver_bench::benches();
}

#[cfg(not(feature = "benchmarking"))]
fn active_benches(_: &mut Criterion) {
    println!(
        "Benchmarks are disabled. Enable with `--features datadog-crashtracker/benchmarking`."
    );
}
criterion_group!(benches, active_benches);
criterion_main!(benches);
