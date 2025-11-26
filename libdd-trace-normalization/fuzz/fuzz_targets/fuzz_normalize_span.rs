#![no_main]

use libdd_trace_normalization::fuzz::{fuzz_normalize_span, FuzzSpan};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|fuzz_span: FuzzSpan| {
    fuzz_normalize_span(fuzz_span);
});
