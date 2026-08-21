# Proposal: catch_unwind at the profiling FFI boundary — callback variant

Status: draft, exploration.

## Problem

Same as the bit-flag proposal: panics that unwind through the profiling FFI
boundary are undefined behavior in C callers, and there is no panic guard on
the new `ProfileStatus`-returning hot-path APIs.

## Proposal

Catch the panic inside the FFI body and route the panic message through a
**globally registered callback**. The returned `ProfileStatus` carries a static
sentinel (`c"libdatadog panicked"`), so the panic path performs no allocation
on the return-value side; the only allocation is whatever the wrapper hands to
the callback.

```c
typedef void (*ddog_prof_PanicHandler)(
    const char *function_name,
    const char *message,
    void       *userdata);

void ddog_prof_set_panic_handler(ddog_prof_PanicHandler cb, void *userdata);
```

Internally, every `ProfileStatus`-returning FFI function wraps its body in
`wrap_with_profile_status!`, which:

1. Runs the body inside `catch_unwind(AssertUnwindSafe(...))`.
2. On `Ok(Ok(()))` → `ProfileStatus::OK`.
3. On `Ok(Err(e))` → existing `ProfileStatus::from(e)` path.
4. On `Err(payload)` → fire the registered callback (if any), then return
   `ProfileStatus::from(c"libdatadog panicked")`.

## Caller pattern (C)

```c
static void on_panic(const char *fn, const char *msg, void *ud) {
    fprintf(stderr, "libdatadog panic in %s: %s\n", fn, msg);
    flag_profile_dead((profile_t *)ud);
}

ddog_prof_set_panic_handler(on_panic, my_profile);

// normal calls — error path is one branch, panic info routes via callback.
ddog_prof_ProfileStatus s = ddog_prof_ProfilesDictionary_insert_function(&id, dict, &fn);
if (s.err) { /* one branch covers panic-as-error too */ }
```

## Pros

- **Single observability point.** Useful for metrics: emit a
  `profiling.ffi.panic` counter directly in the callback, regardless of which
  FFI function panicked.
- **Hot-path callers don't have to learn a new bit.** They handle `err != null`
  the same way they always have; panic vs. recoverable error is a separate
  concern handled out-of-band.
- **Allocation-free return path.** The returned status uses a static `CStr` —
  even when the panic was caused by OOM, the return value itself does not
  allocate. (The callback path may allocate to format the message.)
- Native fit for tracer-side panic reporting: profilers can forward libdatadog
  panics into the tracer's existing crash/error pipeline.

## Cons

- **Global mutable state.** Registration must be thread-safe (atomic pointers).
  Init ordering matters: panics that fire before `set_panic_handler` is called
  are silently swallowed.
- **Fork semantics need thought.** A handler registered in the parent is still
  registered in the child; userdata may not be valid post-fork.
- **Reentrancy hazard.** The callback must NOT call back into any libdatadog
  FFI — that risks reentry on a half-mutated handle. The contract has to be
  documented and ideally enforced (e.g., a thread-local "in panic handler"
  guard that turns reentry into a no-op).
- **Locality is lost.** The callback knows the function name but not which
  `Profile` / `Dictionary` handle was being operated on. Callers that need
  per-handle recovery must encode that themselves via `userdata`.

## Example migration

This PR migrates exactly one function —
`ddog_prof_ProfilesDictionary_insert_function` — to show the shape. If
accepted, every `ProfileStatus`-returning FFI function would follow.

## Sibling proposals

- `r1viollet/profiling-ffi-panic-bit` — bit-flag on `ProfileStatus`, no global
  state.
- `r1viollet/profiling-ffi-panic-bit-and-callback` — both, with the callback as
  an optional observability overlay on top of the bit.
