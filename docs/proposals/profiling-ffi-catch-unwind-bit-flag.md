# Proposal: catch_unwind at the profiling FFI boundary — bit-flag variant

Status: draft, exploration.

## Problem

Panics that unwind through the profiling FFI boundary are undefined behavior in
C callers. The hot-path APIs that return `ProfileStatus`
(`ddog_prof_Profile_add2`, `ddog_prof_ProfilesDictionary_insert_*`, ...) have no
panic guard today — a debug-assert, arithmetic overflow, or allocator-OOM in
inner code unwinds straight into C.

Symbol interning is a frequent panic site in practice: hash-table growth,
string allocation, and id arithmetic all sit on hot paths and can panic.

`libdd-common-ffi`'s `wrap_with_ffi_result!`
([utils.rs:10](../../libdd-common-ffi/src/utils.rs)) already solves this for
functions returning the old tagged-union `Result<T>` / `VoidResult`. The new
`ProfileStatus` type has no equivalent.

## Proposal

Encode "this status is the result of a caught panic" in a new bit on
`ProfileStatus.flags`. The OK representation (`{0, null}`) is unchanged, and
existing C consumers continue to work — they just see another error.

```c
#define DDOG_PROF_STATUS_FLAG_ALLOCATED 0x1
#define DDOG_PROF_STATUS_FLAG_PANIC     0x2

bool ddog_prof_Status_is_panic(const ddog_prof_ProfileStatus *s);
```

Internally, every `ProfileStatus`-returning FFI function wraps its body in
`wrap_with_profile_status!`, which:

1. Runs the body inside `catch_unwind(AssertUnwindSafe(...))`.
2. On `Ok(Ok(()))` → `ProfileStatus::OK`.
3. On `Ok(Err(e))` → existing `ProfileStatus::from(e)` path.
4. On `Err(payload)` → `ProfileStatus::from_panic(payload, function_name!())`,
   which builds a heap message (or falls back to a static `'libdatadog
   panicked'`) and ORs in `IS_PANIC_MASK`.

This mirrors the nested-`catch_unwind` OOM-fallback strategy used by
`libdd-common-ffi`'s `handle_panic_error` so the panic path is allocation-safe.

## Caller pattern (C)

```c
ddog_prof_FunctionId2 id;
ddog_prof_ProfileStatus s = ddog_prof_ProfilesDictionary_insert_function(&id, dict, &fn);
if (s.err) {
    if (s.flags & DDOG_PROF_STATUS_FLAG_PANIC) {
        log_fatal("libdatadog panic: %s", s.err);
        // policy: scrap the profile, it may be corrupt
        ddog_prof_ProfilesDictionary_drop(dict);
    } else {
        log_warn("intern failed: %s", s.err);   // recoverable
    }
    ddog_prof_Status_drop(&s);
}
```

## Pros

- One return type, one inspection point. Caller picks the recovery policy
  per-call.
- Zero-cost on the happy path: OK stays `{0, null}`.
- Panic info is per-call, naturally scoped to the failed operation.
- ABI-stable: just a new bit; old C clients that don't know about it still see
  `err != null` and degrade as a generic error.
- No global state, no init ordering, fork-safe.

## Cons

- Caller has to remember to mask the panic bit if they want to differentiate.
  Default-behavior is "treat panic as any other error" — acceptable but loses
  information unless the caller opts in.
- Adds a knob (`is_panic`) to the public surface every caller now sees.
- Does not, by itself, mark the underlying `Profile` / `ProfilesDictionary` as
  poisoned — caller is on the hook for dropping the handle. See "Open question:
  poisoning" below.

## Open question: handle poisoning

`AssertUnwindSafe` makes the compiler happy; it does not make the invariant
true. After a caught panic, the `Profile` or `ProfilesDictionary` may be in a
half-mutated state.

This proposal does **not** auto-poison the handle. The recommended caller
contract is "on panic, drop and recreate". A follow-up could add an atomic
`poisoned` flag on the handle and have subsequent FFI calls short-circuit to
`ProfileStatus::from(c"profile poisoned by prior panic")` — analogous to
`std::sync::Mutex` poisoning.

## Example migration

This PR migrates exactly one function —
`ddog_prof_ProfilesDictionary_insert_function` — to show the shape. If
accepted, every `ProfileStatus`-returning FFI function would follow.

## Sibling proposals

- `r1viollet/profiling-ffi-panic-callback` — global panic callback, return path
  stays clean.
- `r1viollet/profiling-ffi-panic-bit-and-callback` — both, with the callback as
  an optional observability overlay on top of the bit.
