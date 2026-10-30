# Span concentrator arithmetic model

This TLA+ model records three counterexamples to arithmetic assumptions in
`SpanConcentrator`. Each model deliberately violates its configured invariant:

- `SpanConcentratorBucket.cfg`: narrowing a nonzero `Duration` containing exactly
  `2^64` nanoseconds produces a zero `u64` bucket size.
- `SpanConcentratorFlush.cfg`: retaining one 1-nanosecond bucket while flushing at
  the Unix epoch requires subtracting below zero.
- `SpanConcentratorSpanEnd.cfg`: adding a duration of one to an `i64::MAX` start
  time produces an end time outside the `i64` domain.

TLC cannot represent `2^64` directly, so the executable configurations use a
three-bit abstraction: unsigned values wrap at eight, and signed values range
from -4 through 3. The arithmetic operations and invariants are width-agnostic.
The Rust regression tests replay the corresponding witnesses at their concrete
64-bit widths.

Download `tla2tools.jar` from the TLA+ releases, then run TLC from the repository
root:

```sh
for model in Bucket Flush SpanEnd; do
  java -jar /path/to/tla2tools.jar \
    -config "formal/tla/SpanConcentrator${model}.cfg" \
    formal/tla/SpanConcentratorArithmetic.tla
done
```

All three invocations are expected to exit unsuccessfully with an invariant
violation. Matching Rust regression tests live in
`libdd-trace-stats/src/span_concentrator/tests.rs` and intentionally fail until
the implementation rejects or safely handles each input.
