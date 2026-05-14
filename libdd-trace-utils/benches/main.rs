// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::alloc::System;

use criterion::criterion_main;
use libdd_common::bench_utils::ReportingAllocator;

#[global_allocator]
pub static GLOBAL: ReportingAllocator<System> = ReportingAllocator::new(System);

mod deserialization;
mod serialization;

criterion_main!(
    serialization::serialize_benches,
    deserialization::deserialize_benches,
    deserialization::deserialize_alloc_benches
);
