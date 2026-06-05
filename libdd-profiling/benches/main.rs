// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::criterion_main;

mod add_samples;
mod interning_strings;
mod profiles_dictionary;

criterion_main!(
    interning_strings::benches,
    add_samples::benches,
    profiles_dictionary::benches
);
