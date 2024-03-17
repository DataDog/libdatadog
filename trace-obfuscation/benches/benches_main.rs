// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::criterion_main;

mod credit_cards_bench;
mod redis_obfuscation_bench;
mod replace_trace_tags_bench;
mod sql_obfuscation_bench;

criterion_main!(
    credit_cards_bench::benches,
    redis_obfuscation_bench::benches,
    replace_trace_tags_bench::benches,
    sql_obfuscation_bench::benches,
);
