// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "benchmarking")]
use criterion::criterion_main;

#[cfg(feature = "benchmarking")]
mod receiver_bench;

#[cfg(feature = "benchmarking")]
use datadog_crashtracker::receiver_entry_point_bench as receiver_entry_point;

#[cfg(feature = "benchmarking")]
criterion_main!(receiver_bench::benches);
