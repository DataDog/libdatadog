# Formal counterexample models

## Span concentrator arithmetic

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
    -noGenerateSpecTE -metadir target/tlc \
    -config "formal/tla/SpanConcentrator${model}.cfg" \
    formal/tla/SpanConcentratorArithmetic.tla
done
```

All three invocations are expected to exit unsuccessfully with an invariant
violation. Matching Rust regression tests live in
`libdd-trace-stats/src/span_concentrator/tests.rs` and intentionally fail until
the implementation rejects or safely handles each input.

## Atomic multiset lifecycle

`AtomicMultisetLifecycle.tla` models the separate `used` counter and occupied
slots in crashtracker's lock-free multiset. The configured invariant requires
them to agree after each operation. TLC finds the lifecycle trace
`Insert -> Consume`, after which the implementation has one used entry but no
occupied slots.

Run the model from the repository root:

```sh
java -jar /path/to/tla2tools.jar \
  -noGenerateSpecTE -metadir target/tlc \
  -config formal/tla/AtomicMultisetLifecycle.cfg \
  formal/tla/AtomicMultisetLifecycle.tla
```

The invocation is expected to exit unsuccessfully with
`AccountingConsistent` violated. Its matching Rust regression test is in
`libdd-crashtracker/src/collector/atomic_set.rs`.

## Runtime callback registration

`RuntimeCallbackRegistration.tla` models the callback pointer loaded by a
crash-handler reader while another thread replaces the registered callback.
The implementation frees the replaced allocation immediately. TLC finds the
interleaving `ReaderLoad -> WriterReplace`, which leaves the reader holding a
pointer that is no longer alive.

Run the model from the repository root:

```sh
java -jar /path/to/tla2tools.jar \
  -noGenerateSpecTE -metadir target/tlc \
  -config formal/tla/RuntimeCallbackRegistration.cfg \
  formal/tla/RuntimeCallbackRegistration.tla
```

The invocation is expected to exit unsuccessfully with `ReaderPointerIsValid`
violated.
