# Datadog Profiling FFI Notes

## \#[must_use] on functions

Many FFI functions should use `#[must_use]`. As an example, there are many
Result types which need to be used for correctness reasons:

```rust
#[repr(C)]
pub enum ProfileAddResult {
    Ok(u64),
    Err(Error),
}
```

Then on `ddog_prof_Profile_add` which returns a `ProfileAddResult`, there is a
`#[must_use]`. If the C user of this API doesn't touch the return value, then
they'll get a warning, something like:

> warning: ignoring return value of function declared with
> 'warn_unused_result' attribute [-Wunused-result]

Additionally, many types (including Error types) have memory that needs to
be dropped. If the user doesn't use the result, then they definitely leak.

It would be nice if we could put `#[must_use]` directly on the type, rather
than on the functions which return them. At the moment, cbindgen doesn't
handle this case, so we have to put `#[must_use]` on functions.
