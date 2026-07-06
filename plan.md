# Signal-safe crashtracker — simplification & reuse plan

## Goal

Make `libdd-crashtracker/src/collector_signal_safe/` maximally idiomatic Rust and
share as much as possible with the rest of the crate. Where the existing (std)
collector and shared modules hold logic the signal-safe path re-implements, prefer
extracting a **no_std / async-signal-safe** shared piece over keeping two copies —
even if that means making the existing code more `no_std`-friendly.

## PR scoping (decided up front)

The feature branch is already ~6k insertions ahead of `main`. To keep review surface
sane, the work is split across PRs as follows — the phase numbers below map onto this:

- **This branch/PR**: the feature + **Phase 0 only**. Phase 0 is net-deletion
  (surface trims, dead code), so it *shrinks* the reviewed diff.
- **Follow-up PR(s) after merge**: Phases 1–2 (internal typing + `sys.rs`
  consolidation). Confined to `collector_signal_safe/`.
- **Strictly post-merge, one extraction per PR**: all of Theme A (Phase 3) and
  Phase 4. Every A-item reaches outside `collector_signal_safe/` into the shipping
  std collector — that must not be coupled to the feature landing.

## Hard constraints (do not "simplify" these away)

These bound what "reuse" can mean. Every proposal below respects them.

1. **Async-signal-safety.** The crash path (handler → collector child → emitter) may
   run in a signal handler after memory corruption. No heap allocation, no locks, no
   `getenv`, no `std::io`, no panics. This is why `env_get` walks `environ` by hand
   (`sys.rs:628`), why the emitter uses `heapless` + a `Sink` byte trait instead of
   `std::io::Write`, and why `fork_raw` uses inline `asm!`. These stay bespoke.
2. **no_std under the `collector_signal-safe` feature.** Non-test code is already
   `core`/`heapless`/`serde-json-core`/`rustix` only (verified). Any shared code we
   pull in must compile without `std`/`alloc`. This is the lever: several existing
   modules can be made `no_std` so both collectors use them.
3. **FFI ABI is version-pinned but not back-compat.** `#[repr(C)]`/`#[repr(i32)]`
   layouts and discriminants may change between releases, but the `SignalSafeInitResult`
   ⇔ `InitResult` numeric mapping must stay tested (`collector_signal_safe.rs:201`).
   Wire-format (`DD_CRASHTRACK_*` sections + golden fixture) must stay byte-identical
   unless we deliberately regenerate the golden.

## Theme A — Reuse & sharing across modules (headline work)

### A1. Extract a marker-framing helper over a byte sink *(highest value)*
The `BEGIN-marker / body / newline / END-marker` pattern is written twice: the
signal-safe `emit_json_section`/`put_marker_line` (`emitter.rs:113-136,304-310`) and,
by hand at every section, the std collector (`collector/emitters.rs` `emit_config`
423, `emit_metadata` 440, `emit_kind` 431, `emit_procinfo` 505, `emit_stacktrace`
144…). Both already share the marker *constants* via `protocol.rs`.

- Define one framing helper generic over a minimal byte-push trait
  (`fn put(&mut self, &[u8]) -> bool`, i.e. today's `Sink`).
- Provide two adapters: the heapless `Sink` (signal path) and a thin
  `std::io::Write` shim (std path). `core::fmt::Write` may serve as the idiomatic
  bridge **for the std adapter only** — the signal-safe side keeps the bool `Sink`
  (see B5: `core::fmt` is alloc-free but not panic-free, and must not enter the
  crash path unless `check_signal_safe_symbols.sh` proves no panic machinery is
  pulled in).
- Result: ~15 duplicated begin/end pairs collapse to `section(sink, BEGIN, END, |w| …)`.
- **Split into two PRs**: (1) introduce the helper + adopt in the signal-safe
  emitter, guarded by the golden fixture; (2) rewrite the ~15 call sites in
  `collector/emitters.rs`, guarded by a new std-side output snapshot added first.

### A2. Share `StacktraceCollection` + kill the `WireConfig` shadow
`config.rs:125-142` `WireConfig`/`WireTimeout` hand-reproduces the serde shape of
`CrashtrackerConfiguration` (`shared/configuration/mod.rs:29-46`) field-for-field, and
hardcodes the literal `"EnabledWithSymbolsInReceiver"` (`config.rs:155`) which is a
`StacktraceCollection` variant. This silently drifts if the wire schema changes.

- Make `StacktraceCollection` (already a fieldless `#[repr(C)]` enum) `no_std`-safe and
  reference the variant through its `Serialize` impl instead of a string literal. This
  first bullet delivers most of the de-drift value and is the committed scope.
- **Deferred (own PR, only if the first bullet proves insufficient)**: a shared
  `Serialize` struct rendered by both `serde_json` and `serde-json-core` is *not*
  automatically byte-identical (float formatting, map-key limits differ). If pursued,
  it needs a cross-serializer equality test, and making `shared/configuration`
  `no_std` may cascade through its `Vec`/`String` fields.

### A3. Canonical crash-tag key table (config/metadata de-drift)
The metadata tag set is hardcoded in `emitter.rs:138-176` (`language:native`,
`runtime:native`, `is_crash:true`, `severity:crash`, `service`, `env`, `version`,
`runtime_id`, `runtime_version`, `library_version`, `platform`, `injector_version`)
and assembled independently in the std path via `crash_info/metadata.rs` + the `tag!`
macro + `collector/additional_tags.rs`. Two `Metadata` structs also exist (owned
`crash_info/metadata.rs:9` vs borrowed `report.rs:19`) with two tag builders
(`push_tag` `emitter.rs:51` vs `tag!`).

- Define the canonical tag **keys** once (a shared `const` table) and drive both
  builders from it, so a new/renamed tag can't appear in one path only.

### A4. Single signal-number→name + `signal_has_address` source
si_code naming is already shared correctly (`sig_info.rs:118` delegates to
`shared::signal_names::rust_si_code_name`, and signal-safe re-exports the shared module
verbatim — `collector_signal_safe/signal_names.rs:4`, which is a 1-line pass-through).
Still duplicated:
- signal number→name: `shared/signal_names.rs:5` `rust_signal_name` (9 signals) vs
  `sig_info.rs:62` `SignalNames::from<c_int>` (32 variants).
- `signal_has_address` (`shared/signal_names.rs:34`) vs the `si_addr` match in
  `collector/emitters.rs:711`.

Have `SignalNames::from` and the std emitter's address logic delegate to the shared
`no_std` functions (add a number→enum mapping in `shared`). Then delete
`collector_signal_safe/signal_names.rs` (fold the re-export into `mod.rs`).

Direction matters: unify by **expanding the shared table to the 32-variant set**,
never by shrinking the signal-safe path to the shared 9. Note this changes the *std*
collector's output for uncommon signals, and the golden fixture only guards the
signal-safe wire format — add a std-side output snapshot first, or scope the first PR
to the signal-safe side only.

### A5. Shared `ucontext` → `(ip, sp, fp)` register extraction
`backtrace.rs:56-95` (`arch_seed`, RIP/RBP / x29 seeding) duplicates the register reads
in `collector/emitters.rs:532-547` (`REG_RIP`/`REG_RBP`) and the macOS fp-walk at
`emitters.rs:306-370`. Extract a `no_std` `ucontext_registers(uc) -> (ip, sp, fp)`
helper (near `crash_info/ucontext.rs`) used by both; consider sharing the fp-walk itself
as the non-libunwind fallback.

## Theme B — Internal idiomatic-Rust cleanups

### B1. Replace hand-rolled bitmasks with typed flags (`capabilities.rs`)
`capabilities.rs:8-27` hand-defines 6 capability + 13 degradation `const u32` masks with
`& x != 0` idioms scattered across `handler.rs`, plus a parallel `DEGRADATION_REASONS`
`(mask,&str)` table (`:29-46`) kept in sync by hand. Convert to `bitflags` (already a
transitive workspace dep) or two `#[repr(u32)]` flag types; derive `has`/`note_degraded`/
`get`/`degradations` as methods and attach reason strings to the flag definition. Collapse
the repetitive `if ok { caps|=X } else { degraded|=Y }` in `publish` (`:51-96`) to a small
`probe(cond, CAP, DEG)` helper.

### B2. Collapse the struct-of-arrays statics (`state.rs`, `handler.rs`)
Five lock-step `[_; NSIG]` statics in `state.rs:108-118` (`ORIG_FN`, `ORIG_FLAGS`,
`OWN_SIGNAL`, `APP_HANDLER_PRESENT`, `ORIG_MASKS`) and three more in `handler.rs:51-54`
(`REPEAT_FAULT_PC/ADDR/COUNT`) are indexed together. Fold each group into one
`[SignalSlot; NSIG]` (a struct of atomics), centralizing the `unsafe impl Sync`
boilerplate (`StaticMeta`, `SigMaskStorage`) behind one typed wrapper. Invariant:
every `SignalSlot` field stays an *individual* atomic — readers must tolerate torn
reads across fields exactly as today; do not "simplify" the slot into anything
lock-guarded.

### B3. `Stage` name lookup via the enum, not raw ints
`state.rs:167-179` `current_stage_name` re-matches raw `1..8` ints, duplicating the
`Stage` enum (`:147-159`). Add `Stage::try_from(i32)` + `Stage::name(&self)` and load the
enum, removing the drift risk. The FFI `SignalSafeStage`→`Stage` remap
(`collector_signal_safe.rs:135-145`) and the parallel `SignalSafeStage` enum are a 1:1
shadow — collapse via a match-based `From` impl or a single shared enum. **No
transmute**: int→enum transmute is UB on any out-of-range value and a test only
catches drift you thought to test; a match over contiguous `#[repr(i32)]` variants
compiles to the same identity code anyway.

### B4. Idiomatic loops & duplicate helpers
- C-style `while i < NSIG` loops in `state.rs:181-190,198-208` → `for`/iterator
  (`.iter().filter(..).count()`).
- `fail_init` and `reset_init` (`state.rs:92-98`) are byte-identical → one function.
- `word_at` (`backtrace.rs:18`) → `usize::from_ne_bytes` on an `array_chunks` slice.
- `instruction_pointer` (`backtrace.rs:97`) duplicates the `n==1` seed of
  `backtrace_from_ucontext` (`:112`) → reuse.

### B5. Formatting: drop hand-rolled itoa/hex where safe (`fmt.rs`)
`hex`/`hex_addr`/`hex_u32`/`write_i32` (`fmt.rs:8-62`) manually build digits.
Caution: `core::fmt` is alloc-free but **not panic-free** — the `Formatter`
padding/width machinery has panicking branches, and `write!` on the crash path can
pull `core::panicking::panic_fmt` into code that runs after memory corruption. The
hand-rolled digits exist precisely to avoid that. Preference order:
1. the alloc-free, panic-free `itoa` crate (already a transitive dep) for decimals;
2. keep the hand-rolled hex (or `itoa`-style tables) for `hex`/`hex_addr`;
3. `core::fmt::Write` only if `tools/check_signal_safe_symbols.sh` passing — with no
   new panic/fmt symbols — is a hard gate *on that specific commit*.
Keep a named capacity const for `hex_u32` instead of the bare `10` (`fmt.rs:12`).

### B6. De-duplicate the two `raw` syscall modules (`sys.rs`) *(large win)*
The two `mod raw` blocks (`sys.rs:50-389` vs `395-567`) are ~90% identical — `write`,
`close`, `fcntl_dupfd`, `fd_valid`, `pipe`, `open_readwrite`, `access_executable`,
`mprotect_none`, `getpid`, `gettid`, `kill`, `waitpid_nohang_status`, `poll_sleep_ms`,
`monotonic_nanos` are byte-identical `rustix` calls. Only ~5 functions genuinely differ
per cfg. Move the common ones into one shared module (~170 lines removed). Shrink the
inline-`asm!` `syscall1/3/6` block (`:66-178`) to only what has no stable `rustix`
equivalent: keep `fork_raw` (clone), `read_own_mem` (`process_vm_readv`),
`close_range`, **and `exit_group`** — `rustix::runtime` is experimental,
linux_raw-backend-only, and not in our pinned feature set (`Cargo.toml:93`); five
lines of stable asm beat an unstable feature flag. Route `dup2`→`rustix::io::dup2`
only after confirming its typed-fd signature (`&mut OwnedFd` target) fits the raw-fd
usage in the handler without `OwnedFd` construction gymnastics — otherwise keep the
raw syscall. Replace the manual EINTR loop in `FdSink::put` (`sys.rs:29-41`) with
`rustix::io::retry_on_intr` (confirm it exists in the pinned `=1.1.3` first).
Merge the near-identical `cstr_starts_with`/`env_entry_value` prefix scanners
(`sys.rs:673-700`) into one helper; base `cstr_bytes_bounded` (`:661`) on
`CStr::from_ptr(..).to_bytes()` with the length cap layered on top.

Platform matrix: this file is cfg-dense (the second `raw` module is the
macOS/fallback path; the asm block is per-arch). Require green builds on x86_64 *and*
aarch64, Linux *and* macOS, before and after — the Linux e2e test alone does not
exercise the fallback module.

### B7. Pass structs, not scalar bundles (`handler.rs`)
Two `#[allow(clippy::too_many_arguments)]` sites (`handler.rs:377,401`,
`collector_child`/`emit_crash_report`) thread 8-9 scalars (sig/si_code/has_info/si_addr/
pid/tid/ucontext) that already have homes in `SignalInfo`/`CrashContext`
(`report.rs:38,83`). Build the struct once and pass it. Fold the three repeat-fault
arrays (B2) into the same context. Replace the trivial enum→enum maps `begin_init_error`/
`prepare_error` (`handler.rs:133-145`) with `From` impls.

## Theme C — Public-API surface reduction

`mod.rs` re-exports ~20 items (`mod.rs:43-63`); trim to what the FFI and tests actually
need.

- **Drop the bool wrappers** `init`/`init_from_env` (`handler.rs:69-79`) — the FFI only
  calls the `_result` forms (`collector_signal_safe.rs:76,94`), but
  `tests/collector_signal_safe_e2e.rs:13,26` calls `init_from_env()` directly: switch
  the e2e to `init_from_env_result()` in the same commit. Keep the `InitResult`
  status enum (needed for stable FFI codes); do **not** collapse to `Result`.
- The policy one-liners `app_handler_is_real`, `should_run_app_first`, `app_recovered`
  (`policy.rs:34-44`) are re-exported (`mod.rs:55-56`) but consumed only in `handler.rs`
  — inline or make `pub(super)`.
- Reconsider `SIG_DFL_VALUE`/`SIG_IGN_VALUE` (`policy.rs:8-9`) reimplementing
  `libc::SIG_DFL`/`SIG_IGN`; compare against libc directly.
- Remove the unused `use crate::protocol;` at `mod.rs:26` (protocol is used as
  `protocol::` inside `emitter.rs`, not `mod.rs`).
- Audit `owns_signal`/`owned_signal_count`/`write_i32`/`cstr_bytes_bounded` re-exports —
  keep only those crossing the crate boundary (FFI uses `cstr_bytes_bounded`,
  `owns_signal`, `owned_signal_count`).
- The 24-field manual copy (`collector_signal_safe.rs:94-118`) and the 1:1
  `init_result_to_ffi`/`set_stage` remaps **stay as explicit code** — a mapping macro
  would save ~30 boring lines at the cost of hiding the FFI surface from grep and
  reviewers. Explicit copies are the right idiom at an ABI boundary. Revisit only if
  the enum count grows materially.

## Suggested sequencing

Each phase compiles + tests green before the next (`cargo check -p libdd-crashtracker
--features collector_signal-safe`, then the full validation from AGENTS.md; the golden
fixture and `collector_signal_safe_e2e` guard behavior).

Phases map onto the PR scoping at the top: Phase 0 in this branch; Phases 1–2 as
follow-up PRs; Phases 3–4 strictly post-merge, one extraction per PR.

1. **Phase 0 — dead weight & surface** (low risk, this branch): C-theme trims,
   `mod.rs:26` dead import, `fail_init`/`reset_init` merge, idiomatic loops (B4),
   e2e switch to `init_from_env_result()`. No behavior change; net deletion.
2. **Phase 1 — internal typing**: bitflags (B1), SoA→struct arrays (B2), `Stage` name via
   enum (B3, match-based — no transmute), pass-structs (B7). Local, well-tested by
   existing unit tests.
3. **Phase 2 — sys.rs consolidation** (B6): merge `raw` modules, trim `asm!` (keeping
   `exit_group`), adopt `rustix` helpers. Isolated; guarded by e2e **plus** the
   four-way platform matrix (x86_64/aarch64 × Linux/macOS).
4. **Phase 3 — cross-module reuse** (Theme A): framing helper (A1, split signal-side /
   std-side), `StacktraceCollection` share (A2, first bullet only), tag-key table (A3),
   signal-name unification (A4, expand shared table + std snapshot first), ucontext
   extraction (A5). Requires making a few existing modules `no_std`; one extraction per
   PR; golden fixture byte-stable.
5. **Phase 4 — formatting** (B5): only after A1 lands, so `fmt.rs` is deleted rather
   than rewritten. `itoa`/hand-rolled preferred; `core::fmt` only behind the symbol-check
   gate.

## Validation checklist

- `cargo check -p libdd-crashtracker --features collector_signal-safe`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  (target: remove the two `too_many_arguments` allows and any `dead_code` allows made
  obsolete)
- `cargo nextest run -p libdd-crashtracker --features
  libdd-crashtracker/generate-unit-test-files` incl. `collector_signal_safe_e2e`
- Golden fixture `tests/fixtures/signal_safe_report.golden` unchanged (or regenerated
  intentionally via the `#[ignore]` regenerate test with the diff reviewed).
- `tools/check_signal_safe_symbols.sh` — run **per phase**, not just at the end; it is
  the guard that catches `core::fmt`/panic machinery leaking into the crash path
  (B5/A1) and non-signal-safe symbols from any newly-shared dependency.
- Phase 2 additionally: green builds on x86_64 and aarch64, Linux and macOS.
- If FFI touched: `cargo ffi-test` + `examples/ffi/signal_safe_crashtracking.c`.

## Non-goals / explicitly out of scope

- Rewriting `fork_raw`/`read_own_mem` inline `asm!` — inherent to signal-safety.
- Reusing the std `from_env`/telemetry config machinery — it allocates and calls
  `getenv`; the bespoke `environ` walk stays. (The bespoke bool/log-level/case-insensitive
  parsers in `config.rs:367-418` may be factored into a shared *no_std* helper if a second
  signal-safe consumer appears, but not merged with the std parsers.)
- Changing the wire protocol or the `InitResult` numeric contract.
</content>
</invoke>
