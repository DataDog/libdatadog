// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![no_main]

use libdd_trace_normalization::fuzz::{fuzz_normalize_span, FuzzSpan};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|fuzz_span: FuzzSpan| {
    fuzz_normalize_span(fuzz_span);
});
