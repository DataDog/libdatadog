// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![no_main]

use libdd_trace_normalization::fuzz::fuzz_normalize_strings;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    fuzz_normalize_strings(data);
});
