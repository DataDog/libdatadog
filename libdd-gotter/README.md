# libdd-gotter

> [!WARNING]
> This library does runtime function interposition by patching the Global Offset Table (GOT) of loaded ELF objects. This is a substantial intervention in a running process — it modifies function pointers that the dynamic linker has already resolved, affecting all code that calls through those GOT entries. Incorrect use can cause crashes, infinite recursion, heap corruption, or silent data loss. Understand the ELF dynamic linking model before using this crate.

## What it does

When a shared library calls an external function like `malloc`, it jumps through a pointer in its **Global Offset Table** -- a writable table that the dynamic linker fills at load time. This crate walks every loaded ELF object via `dl_iterate_phdr`, parses its `PT_DYNAMIC` segment, and rewrites GOT entries so calls are redirected to a hook function. The original function address is resolved and returned so the hook can forward to it.

## Usage

### Single-symbol hook (crashtracker intercepting `__assert_fail`)

```rust
use libdd_got_hook::hook_symbol;

static ORIG_FN: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn my_hook(/* same signature as target */) {
    // ... do work ...
    // forward to original via ORIG_FN
}

let mut orig_addr: usize = 0;
unsafe {
    hook_symbol(c"__assert_fail", my_hook as *const () as usize, &mut orig_addr);
}
ORIG_FN.store(orig_addr, Ordering::Release);
```

### Multi-symbol registry (heap profiling hooking malloc/free/calloc/realloc)

See [`libdd-profiling-heap-gotter`](../libdd-profiling-heap-gotter) which builds a `SymbolOverrides` registry on top of the primitives exported by this crate.

## Platform support

- **64-bit Linux (glibc)**: Full support.
- **64-bit Linux (musl)**: Works for dynamically linked symbols. Statically linked symbols have no GOT entries and cannot be patched.
- **Other platforms**: The crate compiles but exports nothing — all types and functions are `cfg`-gated to `target_os = "linux"`.
