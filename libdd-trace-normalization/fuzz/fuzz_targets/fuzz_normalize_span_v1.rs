// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![no_main]

use libdd_trace_normalization::fuzz::{FuzzV1Span, fuzz_normalize_span_v1};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|fuzz_span: FuzzV1Span| {
    fuzz_normalize_span_v1(fuzz_span);
});
