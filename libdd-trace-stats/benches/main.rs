// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use criterion::criterion_main;

mod span_concentrator_bench;

criterion_main!(span_concentrator_bench::benches);
