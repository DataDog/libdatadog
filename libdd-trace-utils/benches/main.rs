// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::alloc::System;

use criterion::criterion_main;
use libdd_common::bench_utils::ReportingAllocator;

#[global_allocator]
pub static GLOBAL: ReportingAllocator<System> = ReportingAllocator::new(System);

mod agentless_encoding;
mod deserialization;
mod deserialization_v05;
mod otlp_encoding;
mod serialization;

criterion_main!(
    serialization::serialize_benches,
    deserialization::deserialize_benches,
    deserialization::deserialize_alloc_benches,
    deserialization_v05::deserialize_v05_benches,
    deserialization_v05::deserialize_v05_alloc_benches,
    agentless_encoding::agentless_encoding,
    otlp_encoding::otlp_benches
);
