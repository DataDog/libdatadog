// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::criterion_main;

mod benchmarks;

criterion_main!(
    benchmarks::credit_cards_bench::benches,
    benchmarks::redis_obfuscation_bench::benches,
    benchmarks::replace_trace_tags_bench::benches,
    benchmarks::sql_obfuscation_bench::benches,
    benchmarks::ip_address_bench::benches,
);
