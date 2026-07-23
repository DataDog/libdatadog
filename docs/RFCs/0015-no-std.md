# RFC 0015: Use `no_std` where it's beneficial for consumers

## Context

`no_std` is, for most of what libdatadog does, the better Rust. Not because we want to run on bare metal, but because the constraints `no_std` imposes — explicit allocation, no hidden global state, no `std::` machinery dragged in transitively — line up almost perfectly with the constraints we are *already* trying to honour as a library that ships into other people's runtimes.

Concretely, four things make `no_std` attractive for this workspace:

1. **Signal safety by construction.** `core` and `alloc` (with a signal-safe allocator) are made of pure functions, integer math, and stack-allocated data. None of `std`'s mutex, thread-local, environment, file-descriptor, or panic-handler machinery is reachable. Code that runs in async-signal contexts — crashtracker, profiling samplers, anything called from a signal handler — is *much* easier to keep correct when `std::` is simply not in the import graph. The compiler enforces what code review otherwise has to.
2. **Smaller artifacts.** Embedders linking libdatadog statically pay for everything `std` pulls in, whether they use it or not. `no_std + alloc` lets us ship the same functionality with substantially less code in the final binary, and noticeably faster compiles in the tree.
3. **Frequently, it's a mechanical change.** A surprising amount of "make this `no_std`" work is replacing `std::` with `core::` and adding `extern crate alloc;`. yaml/yaml-serde#8 is a recent example: a near-mechanical patch turned an `std` crate into a `no_std + alloc` crate without changing its API. Many of our internal crates are in the same shape.

The first concrete driver in this workspace is `libdd-library-config` (prototyped in the sibling worktree `no-std-library-config`), but the case generalises: data structures, parsers, protocol definitions, error types, and signal-handler-adjacent code all benefit. The exceptions — sockets, files, threads, processes — are real but bounded.

This RFC proposes the policy.

## The thesis

**Prefer `no_std + alloc`. Use `std` only where it is earning its keep.**

Concretely, that means:

- For **new crates**, the default should be `no_std + alloc` unless the crate's reason for existing is OS interaction.
- For **existing crates**, `no_std` support is added opportunistically: whenever a crate is touched substantially, or whenever a downstream consumer asks, evaluate whether the migration is cheap. If it is — and for many of our crates it will be — do it.
- For **signal-handler-adjacent code paths** (crashtracker, profiling sample paths, any future async-signal-safe component), `no_std` is the strongly preferred default *for correctness reasons*, not just ergonomics. The compiler refusing to let you call `std::sync::Mutex` from a signal handler is exactly the property we want.

This is opportunistic in the sense that we are not going to stop the world and rewrite the workspace. But dictated by needs of products like profiling, crashtracking or auto_inject, we will attempt to make parts of libdatadog compatible with the constraints of the environment.

## Crate conventions

Crates that opt in follow the same shape so the workspace stays uniform.

**Default to `std` for source compatibility.** Every `no_std`-capable crate keeps `std` in its default features. Adding `no_std` support is a non-breaking change; existing consumers do not need to know.

```toml
[features]
default = ["std"]
std = [
    "serde/std",
    "anyhow/std",
    "dep:libc",
    "dep:memfd",
    # ... and any optional deps that only make sense with std
]
```

**Crate root.** Conditional `no_std`, unconditional `alloc`. We rely on a heap; we do not target true bare-metal.

```rust
#![cfg_attr(not(feature = "std"), no_std)]
extern crate alloc;
```

**Imports.** Use `core::` and `alloc::` everywhere they exist. Gate genuinely `std`-only items behind `#[cfg(feature = "std")]`:

```rust
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::OnceCell;

#[cfg(feature = "std")]
use std::path::Path;
```

**Dependencies.** Every dependency is declared `default-features = false`. Anything the dependency only exposes under its own `std` feature is forwarded through this crate's `std` feature. Optional dependencies that are inherently `std` (`libc`, `memfd`, `prost`, etc.) live behind `dep:` in the `std` feature list.

**Errors.** `thiserror` v2 and `anyhow` (with `default-features = false`) work in `no_std` and should be preferred over hand-rolled error enums.

## Workspace enforcement

When a crate opts in:

- CI builds it with `--no-default-features` in addition to the default build. Without this, a careless `use std::` lands and silently breaks embedders.
- The crate's `README.md` documents `no_std` support and how to disable `std`.
- Reviewers treat a broken `--no-default-features` build the same as a broken default build.

For crates that have not opted in, none of this applies, and reviewers do not block PRs on it. The policy is opt-in, not retroactive.

## Initial candidates

Strong candidates, evaluated and migrated in follow-up PRs:

- `libdd-library-config` — already prototyped on `no-std-library-config`. Reference implementation.
- **`libdd-crashtracker` (the collector half).** This is the most interesting case. The crash-time code path runs in a signal handler and must be async-signal-safe. A `no_std` collector half — where the compiler refuses to let you reach for `std::sync::Mutex` or `eprintln!` — is meaningfully *safer by construction* than the current crate, independent of any embedder request. The reporting/serialisation half that runs post-crash in a separate process can stay `std`. Splitting the crate along that line is a separate piece of design work, but the `no_std` argument is the forcing function.
- `libdd-tinybytes` — small, dependency-light building block.
- `libdd-trace-protobuf` — generated code; should be near-mechanical.
- `libdd-ddsketch` — pure data structure.
- `libdd-otel-thread-ctx` — small surface, plausible embedder need.

Crates that are out of scope by nature — their reason for existing is OS interaction: `datadog-sidecar*`, `datadog-ipc*`, `libdd-shared-runtime*`, `libdd-http-client`, `libdd-data-pipeline`, `spawn_worker`, all `*-ffi` shells. These stay `std`.

## Drawbacks

- **Build matrix grows.** Each opted-in crate adds a `--no-default-features` build to CI. Real but bounded.
- **no_std rust ecosystem is large, but not all crates support it.** Code willing to support `no_std` might have to do extra work to align dependencies with `no_std` requirements.
- **Cognitive overhead in opted-in crates.** Contributors have to use `core::`/`alloc::` and gate `std`-only code. We consider this a feature: it forces the same discipline we'd want at code-review time anyway.
- **Adding a dependency becomes a small research task.** Does it support `no_std`? With which features? Mostly this is good — it discourages casual dependency growth — but it is friction.
- **Forks accumulate maintenance debt.** This risk is real and should be weighed carefully before adopting or maintaining a fork.

## Alternatives considered

- **Workspace-wide `no_std` mandate.** Rejected: forces awkward abstractions onto crates whose domain is genuinely OS-bound, with no benefit.
- **Never go `no_std`.** Rejected: gives up the signal-safety, binary-size, and dependency-hygiene wins; blocks embedder use cases that are already arriving.
- **Parallel `*-core` crates per opt-in.** Rejected: source duplication, split issue trackers, two places to land every fix.

## Recommended

Adopt the policy: allow and recommend `no_std + alloc`; use `std` mainly where it is earning its keep. Land `libdd-library-config` `no_std` support as the reference implementation, including the CI shape and the conventions above. Schedule `libdd-crashtracker` as the next target on signal-safety grounds.
