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
// Release pairs with the Acquire load in my_hook, ensuring the GOT
// patches from hook_symbol are visible before the hook reads orig_addr.
ORIG_FN.store(orig_addr, Ordering::Release);
```

### Multi-symbol registry (heap profiling hooking malloc/free/calloc/realloc)

See [`libdd-profiling-heap-gotter`](../libdd-profiling-heap-gotter) which builds a `SymbolOverrides` registry on top of the primitives exported by this crate.

## Support
This library can be used on ARM64 and AMD64 Linux in processes using glibc or musl runtimes.
Only symbols that have been dynamically linked can be intercept.
For instance, if you want to intercept the `malloc` of your C runtime,
you _cannot_ do so if the application has been statically linked against musl.
