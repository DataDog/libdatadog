// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::criterion_main;

mod add_sample_vs_add2;
mod interning_strings;

criterion_main!(interning_strings::benches, add_sample_vs_add2::benches);
