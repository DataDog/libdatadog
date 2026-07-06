# Plan: simplify and clean up the signal-safe crashtracker

Theme: **code reuse and sharing**. The signal-safe collector (`libdd-crashtracker/src/collector_signal_safe/`, ~4,000 lines) currently shares almost nothing with the rest of the workspace except the wire protocol constants (`protocol.rs`) and two freshly split shared modules (`shared/signals.rs`, `shared/defaults.rs`). Everything else — syscall wrappers, signal-name tables, config wire schema, metadata tag sets, fork/reap logic — exists twice. This plan removes internal duplication first, then makes the signal-safe primitives the single source of truth that the std collector and `libdd-common` reuse, making other code more no_std-compatible where needed.

What is already right and should stay as-is (the model to follow):

- `protocol.rs` is the single source of the `DD_CRASHTRACK_*` markers; both emitters and the receiver parser consume it, verified by the round-trip test in `receiver/mod.rs:109-181`.
- `signal_owner.rs` is a minimal, no_std-clean arbiter shared by both collectors.
- The two **emitters** and two **backtrace strategies** stay separate by design (see Non-goals).

---

## Phase 1 — In-module cleanup (no cross-module churn, no behavior change)

Quick deletions and consolidations inside `collector_signal_safe/` and the FFI crate.

1. **`fmt.rs`**: collapse `hex_addr` and `hex_u32` (`fmt.rs:8-40`) — byte-identical nibble loops — into one generic helper (`fn hex<const N: usize>(value: u64, digits: usize)` or two thin wrappers over a shared core).
2. **`config.rs`**: delete `eq()` (`config.rs:372`) — `&[u8] == &[u8]` works in core. Keep only `eq_ic` (case-insensitive), which is genuinely needed.
3. **`policy.rs`**: remove the dead `SignalContext` struct and its `is_genuine_fault` method (`policy.rs:27-38`) plus the `mod.rs:55` re-export. Production only calls the free functions; port its tests to the free function.
4. **`sys.rs`**: remove the never-called `syscall0`, and remove the blanket `#![allow(dead_code)]` at `sys.rs:4` so future dead code is caught by the compiler instead of hidden.
5. **`handler.rs`**: factor the shared body of `init_result` / `init_from_env_result` (`handler.rs:73-132`) — they differ only in the `prepare_*` call; take the prepared config as a parameter. Merge the overlapping `reset_handlers_to_default` / `reset_signal_to_default` (`handler.rs:293-306`) into one function over a slice.
6. **`emitter.rs`**: consolidate the three near-parallel entry points (`emit_report`, `emit_minimal_report`, `emit_report_with_metadata`, `emitter.rs:102-147`) into one section-sequence driver parameterized by what is available (frames present or not, metadata source). One place defines section order; the variants become thin wrappers.
7. **CSTR reader dedup**: `sys.rs:651 cstr_bytes_bounded` and `libdd-crashtracker-ffi/src/collector_signal_safe.rs:193 cstr_bytes` are the same bounded loop (same `CSTR_MAX_LEN = 4096`). Export one from the library crate and use it from FFI.

Estimated deletion: ~150–200 lines, plus a compiler-enforced dead-code guarantee going forward.

## Phase 2 — Shrink `sys.rs` by finishing the rustix migration

`sys.rs` (743 lines) is inconsistent: `write/close/pipe/getpid/gettid/nanosleep/clock_gettime` already go through **rustix** (`sys.rs:272-457`), while `dup3, fcntl, close_range, openat, faccessat, mprotect, kill, wait4, process_vm_readv` still use hand-written `asm!` wrappers `syscall1..syscall6` (`sys.rs:67-270`). rustix's linux-raw backend issues raw syscalls (no libc, async-signal-safe), so the split buys nothing.

1. Route all remaining wrappers through rustix (already a dependency with the right feature set, `Cargo.toml:96`). Verify each call is available in the pinned rustix version and covered by the enabled features (`event,fs,pipe,process,stdio,thread,time` — may need `mm` for mprotect and `process` for pidfd/wait variants).
2. **Keep exactly one raw-asm path**: `fork_raw` via raw `clone(SIGCHLD)` (`sys.rs:368-401`) — libc `fork()` runs atfork handlers, and rustix deliberately doesn't wrap clone-as-fork. Document why it is the sole exception.
3. Delete the now-unused `syscallN` layer and its per-arch `asm!` blocks.
4. **Replace the hand-rolled `environ` walker** (`sys.rs:623-690`, consumed by `config.rs:251-283`): `prepare_from_env` runs at **config time**, not in the signal handler, so the async-signal-safe raw-`environ` machinery is over-built. Use `libc::getenv` (still no_std-friendly, no alloc) or `std::env::var_os` behind the existing std test gate. Keep `strip_loader_injection_env` (`handler.rs:354-374`) as-is — that one genuinely runs in the fork child and must stay raw.
5. Extend `tools/check_signal_safe_symbols.sh` to the post-migration symbol set so the rustix migration is proven not to pull in banned symbols (malloc, pthread locks, stdio) on the crash path. Run it before and after as the acceptance gate for this phase.

Estimated deletion: ~300+ lines of asm and env machinery; `sys.rs` becomes mostly a thin, documented rustix facade plus `fork_raw`.

## Phase 3 — Promote `sys.rs` to the shared signal-safe primitive layer

This is the biggest overlap: the old collector and `libdd-common/unix_utils` maintain a second, *less safe* implementation of fork/exec/reap that the improvement doc (`crashtracker-work-we-need-to-do.md:82-116`) already flags as "audit or replace".

1. **Placement**: keep the primitives inside `libdd-crashtracker` initially — move `collector_signal_safe/sys.rs` (post Phase 2) to `libdd-crashtracker/src/signal_safe_sys/` (crate-visible, compiled whenever either collector feature is on). Promoting to a standalone crate (`libdd-signal-safe`) is deferred until a second crate actually needs it; don't create a crate on speculation. `libdd-common` is the wrong home — it is std/tokio-heavy and the dependency arrow should point the other way.
2. **Retire crash-path usage of `libdd-common/unix_utils` from the std collector**, replacing with the shared primitives. This kills three known bugs for free:
   - `collector_manager.rs:100-102` closes fds 0/1/2 before writing to `uds_fd`, destroying the crash socket if it landed on a low fd → use `sanitize_clone`-style `F_DUPFD` relocation (`handler.rs:259-291`).
   - `run_collector_child` never resets crash signals to `SIG_DFL` → reuse the shared reset helper.
   - `eprintln!` on signal/fork-child paths (`signal_handler_manager.rs`, `collector_manager.rs`, `process_handle.rs`) → reuse the fixed-buffer `crash_debug` raw-write path (`handler.rs:243-257`).
   - `fork.rs::is_being_traced` (std::fs::File + UTF-8 parse on the signal path) → raw read via the shared primitives or drop from the crash path.
3. **Reap semantics**: replace `libdd-common/process.rs` busy-wait reap on the crash path with the bounded `reap_or_kill` (`handler.rs:490-515`).
4. `libdd-common/unix_utils/{fork,execve,process}.rs` keep their std API for non-crash users, but their crash-path callers are gone; mark them accordingly (doc comment: not async-signal-safe, must not be called from signal context).

Note: this phase changes old-collector behavior (fixes bugs, changes reap timing). It needs its own PR with the e2e suites of *both* collectors green, and is the natural place to add the doc's missing child-sanitation tests for the std collector.

## Phase 4 — Single source of truth for shared data (tables, schemas, mirrors)

Each of these is currently two definitions kept in sync only by a test. Invert that: one definition, and the test becomes a compatibility check against the receiver, not a sync check between twins.

1. **Signal/si_code names**: `collector_signal_safe/signal_names.rs` (pure const matches, no_std) vs `crash_info/sig_info.rs` (`SignalNames`/`SiCodes` serde enums backed by C code in `emit_sicodes.c`), synced only by `receiver/mod.rs:183-246`. Make `signal_names.rs` the canonical no_std table (move to `shared/`), and have `crash_info`'s human-readable strings derive from it — ideally letting the C `translate_si_code_impl` path retire. Keep the receiver round-trip test.
2. **Wire config schema**: `config.rs:122-139 WireConfig` is a no_std mirror of `CrashtrackerConfiguration`'s serialized form, synced by golden-bytes tests. Define the wire schema once as a no_std serializable struct in `shared/`, have `CrashtrackerConfiguration` convert into it, and serialize the same struct with `serde_json` (std path) and `serde_json_core` (signal-safe path). The golden test then guards the wire contract, not twin drift.
3. **Metadata tag set**: `emitter.rs:196-234` hardcodes the dd-trace-c native tag list; `crash_info::Metadata` has the overlapping semantics. Extract a shared tag-name/order definition (const slice of tag keys in `shared/`) that both the signal-safe emitter and any future std-side preload-metadata builder consume (this also pre-builds doc item 10).
4. **FFI mirror enums**: `InitResult`/`Stage` vs `SignalSafeInitResult`/`SignalSafeStage` are field-for-field mirrors synced by a value-equality test (`collector_signal_safe.rs:209`). Since the C FFI explicitly has no ABI-stability guarantee (per AGENTS.md), make the library enums `#[repr(C)]` and re-export them through cbindgen directly, deleting the mirrors and the sync test. Same consideration for `SignalSafeConfig` vs `SignalSafeInitConfig` — the lifetime-carrying `&[u8]` fields likely force keeping the config mirror, but the enums don't need one. Regenerate cbindgen headers and run `cargo ffi-test`.
5. **`protocol.rs` dead-marker hygiene**: the `#[allow(dead_code)]` on unused markers (WHOLE_STACKTRACE, COUNTERS, …) is fine — they're the shared protocol used by the std emitter — but move the allow to the macro call sites that need it rather than blanket, so genuinely dead markers surface.

## Phase 5 (optional, behavior change) — std collector adopts `policy.rs`

`policy.rs` is pure, no_std, heavily tested chaining/genuine-fault logic (re-fault vs re-raise, `SI_USER`/`SI_TKILL` filtering, Mode A/B). The old `signal_handler_manager.rs:113-130` chaining is strictly weaker (always `raise`, no genuine-fault filter — risk #5 in the improvement doc). Having the std collector call into `policy.rs` closes that gap with shared, already-tested code.

This changes what the std collector reports (external `kill` no longer becomes crash telemetry) and how it chains (re-fault on synchronous faults). It's the right direction per the improvement doc, but it is a **product behavior change**, not a cleanup — do it as its own PR, opt-in or with explicit sign-off, after Phases 1–4.

## Non-goals (explicitly out of scope)

- **Do not merge the two emitters.** The std emitter (`collector/emitters.rs`: 15 sections, `/proc` reads, serde_json) and the signal-safe emitter (fixed minimal section list, serde-json-core, heapless) serve different contracts; they already share `protocol.rs`, which is the correct amount of sharing.
- **Do not make the libunwind backtrace path signal-safe** or unify it with the frame-pointer/`process_vm_readv` walk. Different strategies by design (improvement doc item 11).
- **Do not port `saguard.rs`'s RAII pattern into the signal-safe path** — its `Drop`-based restore is exactly the `siglongjmp` hazard the new code avoids with explicit enter/leave markers.
- **No new crate yet** (see Phase 3 placement).
- Feature work from `crashtracker-work-we-need-to-do.md` (sigaction PLT interposition, receiver path discovery, seccomp probing, packaging) is tracked there, not here.

## Sequencing and validation

Suggested PR split (each independently green):

1. Phase 1 (pure cleanup) — one PR.
2. Phase 2 (rustix migration + env walker removal) — one PR, gated on `tools/check_signal_safe_symbols.sh` before/after plus the golden fixture test (`tests/fixtures/signal_safe_report.golden`) staying byte-identical.
3. Phase 3 (shared primitives + std-collector adoption) — one PR, both collectors' e2e suites green (`collector_signal_safe_e2e.rs` and the std collector tests).
4. Phase 4 — can be split per item (4.1 signal names, 4.2 wire config, 4.4 FFI enums are independent).
5. Phase 5 — separate, explicit sign-off.

Per-change validation (per AGENTS.md):

```bash
cargo check -p libdd-crashtracker
cargo +nightly-2026-02-08 fmt --all -- --check
cargo +stable clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run -p libdd-crashtracker --features libdd-crashtracker/generate-unit-test-files
cargo nextest run --workspace --no-fail-fast
cargo test --doc
cargo ffi-test                      # Phases 3–4 touch FFI
./tools/check_signal_safe_symbols.sh   # every phase — the crash-path symbol guard is the safety net
```

Invariants that must hold at every step:

- The emitted wire bytes stay identical (golden fixture) unless a phase explicitly says otherwise — none of Phases 1–4 should change the wire format.
- No new symbols on the crash path: no alloc, no locks, no stdio, no `Drop`-bearing state across the app-first call.
- The signal-safe module keeps compiling with only its declared no_std-friendly deps (`heapless`, `libc`, `rustix`, `serde`, `serde-json-core`); anything moved into `shared/` for reuse must itself stay no_std-clean (core-only, no `std::` imports) so the sharing direction is std-code-depends-on-no_std-code, never the reverse.
