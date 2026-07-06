# Signal-safe crashtracker: simplification & code-reuse plan

Theme: shrink `collector_signal_safe` (~4,000 lines / 12 files) by deleting dead
surface, replacing hand-rolled code with crates already in the dependency tree
(`rustix`, `libc`), and moving genuinely shared logic into the crate's existing
shared no_std layer (`protocol.rs`, `shared/`, `signal_owner.rs`) so both
collectors consume one copy. Where sharing requires it, the *other* code (std
collector, `crash_info`) is made more no_std — not the reverse.

## Ground rules

- The `collector_signal-safe` feature must keep working **without** `std`
  (Cargo.toml:61 pulls only `heapless`, `libc`, `rustix`, `serde`,
  `serde-json-core`). Anything shared with it must be no_std.
- Code reachable from `crash_handler` (handler.rs), `backtrace.rs`,
  `emitter.rs`, `fmt.rs`, `sys.rs::raw`, and `capabilities::has/note_degraded`
  runs in the signal handler or the forked child: replacements must be
  async-signal-safe. rustix's `linux_raw` backend (direct syscalls, no libc
  PLT, no locks) qualifies — the module already uses rustix in the handler for
  `write`/`close`/`pipe`/`getpid`/`gettid`/`nanosleep`/`clock_gettime`, which
  is the precedent.
- Wire format is the contract. The guards already exist and must stay green
  throughout: the golden fixture test (`mod.rs::emitted_wire_matches_golden_fixture`),
  the receiver round-trip test (`receiver/mod.rs:109`), the signal-name
  compatibility test (`receiver/mod.rs:183`), and the e2e test
  (`tests/collector_signal_safe_e2e.rs`).
- **"Dead" is per-target.** The support matrix (mod.rs:10-17) has three tiers
  (Linux x86_64/aarch64, other-Linux, macOS/iOS). Before deleting anything,
  confirm it is unused under every cfg: `cargo check` for
  `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, an "other-arch"
  Linux target, and `aarch64-apple-darwin`. In particular
  `emit_minimal_report` is the macOS fallback per the support matrix — verify
  before touching it.

---

## Phase 1 — Shrink the public surface, delete dead code (low risk, do first)

The real external surface (used by `libdd-crashtracker-ffi/src/collector_signal_safe.rs`,
`lib.rs`, and production `receiver/`) is only ~15 symbols:
`SignalSafeInitConfig`, `init`, `init_result`, `init_from_env`,
`init_from_env_result`, `bootstrap_complete`, `shutdown`, `InitResult`,
`set_stage`, `Stage`, `cstr_bytes_bounded`, `capability_bits`,
`degradation_bits`, `owned_signal_count`, `owns_signal`.

1. Trim the `pub use` block in `collector_signal_safe/mod.rs:43-65` to that
   list. Demote everything else to `pub(crate)` (receiver tests and mod.rs
   tests are in-crate, so `pub(crate)` suffices for: `emit_report`, `Sink`,
   `SliceSink`, `push_tag`, the `report.rs` structs, `fmt.rs` helpers,
   `policy.rs` items, `rust_signal_name`/`rust_si_code_name`).
2. Delete outright (after the per-target cfg check above):
   - `config::prepare` and `config::prepare_from_env` bool wrappers
     (config.rs:171, 247) — all callers use the `_result` variants.
   - The 7 unused `FPE_*` constants (signal_names.rs:102-107, all but
     `FPE_INTDIV`) — `signal_specific_si_code_name` deliberately maps FPE to
     `UNKNOWN`. (Superseded by Phase 3 if the constants move to libc anyway.)
   - `emit_report_with_metadata` (emitter.rs) — no callers found on any
     target; its truncation tail duplicates `emit_truncated_tail`
     (emitter.rs:415-429).
   - `emit_minimal_report` **only if** the cfg sweep shows the macOS path
     doesn't call it.
3. After (2), collapse the `SectionSequence` / `MetadataSource` /
   `AdditionalTags` indirection (emitter.rs:56-76, 178-226) into the remaining
   live driver(s). This machinery only exists to serve three entry points; with
   one or two left it should be straight-line code.
4. Micro-dedup in `handler.rs` / `capabilities.rs`:
   - Fold the copy-pasted close-stdio arms in `sanitize_clone`
     (handler.rs:261-270) into one block.
   - Extract one "waitpid(WNOHANG) + monotonic deadline + SIGKILL + reap"
     helper and use it from both `reap_or_kill` (handler.rs:462-487) and
     `probe_process_vm_readv_in_child` (capabilities.rs:120-155); replace the
     tail recursion in `reap_or_kill` with a loop.
   - Inline the `disposition_of_target` pass-through (handler.rs:686-688).
   - Funnel the repeated `mem::zeroed::<libc::sigaction>()` + query pattern
     (handler.rs:690, 743, 777, …) through the existing `query_sigaction`.

Estimated effect: several hundred lines gone, emitter.rs substantially
simpler, public API honest.

## Phase 2 — Replace hand-rolled sys.rs with rustix/libc (the big reuse win)

`sys.rs` (708 lines) contains hand-written inline-asm `syscall1..6` for
x86_64/aarch64 plus a second, libc-based portable copy of the same API. rustix
is already a dependency and already used in the handler; the asm duplicates it.

1. Migrate these wrappers to rustix (both the Linux-asm and portable copies
   collapse into one implementation each):

   | sys.rs item | replacement |
   |---|---|
   | `dup2`/`dup3` (sys.rs:256) | `rustix::io::dup2` |
   | `fcntl_dupfd` (sys.rs:263) | `rustix::io::fcntl_dupfd` |
   | `fd_valid` (sys.rs:274) | `rustix::io::fcntl_getfd` |
   | `close_range_from` (sys.rs:278) | `rustix::io::close_range` (`fs` feature, enabled) |
   | `open_readwrite` (sys.rs:301) | `rustix::fs::openat` |
   | `access_executable` (sys.rs:313) | `rustix::fs::accessat` |
   | `kill` (sys.rs:390) | `rustix::process::kill_process` |
   | `waitpid_nohang_status` (sys.rs:394) | `rustix::process::waitpid` |
   | `exit_process` (sys.rs:374) | keep `libc::_exit` / `exit_group` raw (trivial) |

2. `mprotect_none` (guard page): add the rustix `mm` feature to the
   crashtracker's rustix dependency and use `rustix::mm::mprotect`, removing
   another raw syscall. (Check feature-unification cost across the workspace;
   if `mm` is unacceptable, keep this one raw.)
3. Keep genuinely uncovered syscalls raw, but shrink the asm layer to exactly
   what they need: `fork_raw`/`clone(SIGCHLD)` (rustix `runtime` is unstable)
   and `read_own_mem`/`process_vm_readv` (no rustix wrapper). That means one
   or two `syscallN` helpers instead of six, or `libc::syscall` where
   acceptable.
4. Delete the `errno`/`set_errno`/`__errno_location` shim (sys.rs:657-688) —
   rustix returns `Errno` values directly; the only consumer is the portable
   `raw::write` fallback, which moves to rustix too.
5. Replace the custom `IoVec` struct (sys.rs:424-428) with `libc::iovec`.
6. Verification for this phase, beyond the test suite:
   - `tools/check_signal_safe_symbols.sh` must stay clean (this is the guard
     that no libc PLT/alloc symbols crept into the handler path).
   - e2e test on Linux; cross-`cargo check` for the target matrix.

Estimated effect: sys.rs drops from ~700 to roughly 250-300 lines, the entire
per-arch `syscall2`/`syscall4` asm and the duplicated portable API copy go
away, and the module stops maintaining its own syscall ABI knowledge.

## Phase 3 — One source of truth for signal/si_code naming (sharing theme)

Today there are three parallel implementations, held together only by tests:
- std collector/receiver: `SignalNames`/`SiCodes` enums + C-backed
  `translate_si_code` (`crash_info/sig_info.rs:63-256` + `emit_sicodes.c`);
- signal-safe: `rust_signal_name`/`rust_si_code_name` + ~35 hand-defined
  numeric constants (`collector_signal_safe/signal_names.rs`);
- the compatibility test `receiver/mod.rs:183-246` asserting the two agree.

Steps:
1. Source the numeric constants from `libc` (`libc::SEGV_MAPERR`,
   `libc::SI_USER`, `libc::BUS_ADRALN`, …) instead of hand-defining them.
   Keep the existing non-Linux `SI_TKILL` sentinel (signal_names.rs:80-82).
   `libc` with `default-features = false` is no_std, so this costs nothing.
2. Move the name-mapping functions (`rust_signal_name`, `rust_si_code_name`,
   `signal_has_address`) to the shared no_std layer — `shared/signal_names.rs`,
   gated like `shared/signals.rs` so both `collector_signal-safe` and
   `std`/`receiver` builds compile it.
3. Make the std side consume it ("make the other code more no_std"): replace
   the C `translate_si_code_impl` FFI (`emit_sicodes.c`) with the shared Rust
   mapping — `SiCodes`/`SignalNames` parse the shared strings or are generated
   from the same table. This deletes a C file, a build-script compilation
   unit, and the cross-language duplication. The existing compat test flips
   from "two copies agree" to a plain unit test of the one copy.
4. Close the FPE gap while here: add the `FPE_*` variants to the receiver's
   `SiCodes` model, then drop the "FPE reported as UNKNOWN" carve-out
   (signal_names.rs:63-65).

Step 3 is the largest item in this phase and can ship separately after 1-2;
it touches receiver-side data models, so the round-trip and errors_intake
tests are the guards.

## Phase 4 — Optional sharing, only if it pays (evaluate, don't force)

Ranked by value; each is skippable without hurting the phases above.

1. **macOS frame-pointer walk**: the std collector's
   `emit_macos_backtrace_from_ucontext` (collector/emitters.rs:306-368)
   hand-rolls the same FP-chain walk as `collector_signal_safe/backtrace.rs`
   (which additionally reads memory safely via `process_vm_readv`). Move the
   walk into a shared no_std module and have the std collector call it —
   deletes the weaker duplicate.
2. **fmt.rs**: `write_i32` is a hand-rolled `itoa`; the crate isn't in the
   tree. 30 lines of tested code vs. a new dependency — recommend **keep**
   as-is, just `pub(crate)`.
3. **Alt-stack + sigaction machinery**: the std versions
   (signal_handler_manager.rs, saguard.rs) are nix/std-based by design; the
   signal-safe versions exist precisely because those aren't
   async-signal-safe. **Do not unify** — document the split instead.
4. **`probe_process_vm_readv_in_child`** (capabilities.rs:120): forks a child
   at init to detect seccomp filtering, behind an off-by-default flag. If no
   SDK asks for it, delete; otherwise leave (it's init-time only).

## Sequencing & validation

Order: Phase 1 → Phase 2 → Phase 3 (steps 1-2, then 3-4) → Phase 4 cherry-picks.
Each phase is a separate conventional commit
(`refactor(crashtracker): …` / `feat(crashtracker): …` for the FPE addition).

After every phase:

```bash
cargo check -p libdd-crashtracker --no-default-features --features collector_signal-safe
cargo check -p libdd-crashtracker            # default features
cargo +nightly-2026-02-08 fmt --all -- --check
cargo +stable clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run -p libdd-crashtracker --features libdd-crashtracker/generate-unit-test-files
cargo nextest run --workspace --no-fail-fast
./tools/check_signal_safe_symbols.sh
```

Cross-target cfg sweep (needed for the "dead per-target" checks in Phases 1-2):
`cargo check` for `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`aarch64-apple-darwin`, and one non-FP-walk Linux arch. If FFI files are
touched (they shouldn't be — the ~15-symbol surface is preserved), run
`cargo ffi-test`. If `Cargo.lock` changes (rustix `mm` feature does not add
crates, but verify), regenerate `LICENSE-3rdparty.csv`.

## Out of scope

- Changing the wire protocol or receiver parsing (beyond the additive FPE
  enum variants in Phase 3.4).
- Unifying the fork/exec/reap machinery with `libdd_common::unix_utils` — the
  std versions allocate and are not async-signal-safe; that is the reason the
  signal-safe module exists.
- Any change to the `signal_owner.rs` arbitration or the FFI ABI.
