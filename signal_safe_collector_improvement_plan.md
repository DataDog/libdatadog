# Signal-Safe Crashtracker Collector — Improvement Plan

Status date: 2026-07-06. Branch: `signal_safe_crashtracker` (HEAD `dd9922905`,
merge of `6ae6136f3` which restored the previously missing files).

This plan is written to be executed by another engineer/LLM with no prior context.
It covers: restoring the branch to a compilable state, hardening the existing
signal-safe collector, missing-capability handling, safety options, the test
strategy, and reuse/compatibility with the existing crashtracker and the rest of
libdatadog. The dd-trace-c parity analysis lives in
`crashtracker-work-we-need-to-do.md`; this plan references it but does not repeat
it. Where the two overlap, this plan is the actionable order of work.

---

## Guiding principles

Applied throughout this plan (and to be applied to the code):

- **Errors never pass silently.** Every degraded path must leave a trace:
  a `report_degraded:*` tag, a distinct `InitResult` variant, or a debug
  breadcrumb. A crash report that silently loses its stack, or an `init` that
  returns a bare `Failed`, is a bug even when the behavior is otherwise correct.
- **Explicit is better than implicit.** Env reads happen only in the two
  `*_from_env` entry points; everything else takes explicit config. Failure
  reasons are enum variants, not booleans. Options that interact
  (`create_alt_stack`/`use_alt_stack`) are validated, not auto-corrected.
- **One obvious way.** Where earlier drafts offered alternatives, this plan now
  commits to one approach per problem and records why the alternative lost.
  The remaining genuinely-open questions are in the decision table below — in
  the face of ambiguity, confirm with the team instead of guessing.
- **Simple over complex, and explainable.** The crash path stays a straight
  line: probe at init, one handler, two forked children, fixed buffers, no
  hidden state machines. Anything hard to explain in a doc comment (the 1.1
  stack-position heuristic is the one exception) gets that doc comment.

### Open decisions

Everything else in this plan is decided; these need a human call before the
affected PR:

| # | Decision | Recommendation | Blocks |
|---|---|---|---|
| D1 | Ship `collector_signal-safe` in the released FFI artifact now, or keep opt-in? | Team/product call — packaging §5.5 | PR 8 |
| D2 | If re-init after `shutdown()` turns out unsound during the `Meta` audit (1.4), fall back to documented one-shot + `AlreadyInitialized`? | Attempt re-init first; fall back only with a written reason | PR 3 |
| D3 | macOS: stay degraded-fd-only, or add best-effort libc `fork()` mode later? | Stay degraded; revisit only on demand (§2.5) | nothing now |

## 0. Orientation

### What exists on this branch

New module `libdd-crashtracker/src/collector_signal_safe/` behind cargo feature
`collector_signal-safe` (note the hyphen), designed to coexist with the standard
`collector` feature:

| File | Contents |
|---|---|
| `mod.rs` | Wire emitter (`emit_report`, `Sink`/`SliceSink`), policy pure functions (`chain_action`, `is_genuine_fault`, `should_run_app_first`, `app_recovered`), signal/si_code naming, capacity constants |
| `config.rs` | `SignalSafeInitConfig`, `prepare()`/`prepare_from_env()`, config-JSON builder, env parsing (`DD_CRASHTRACKING_*`, `DD_SERVICE`, …), compat presets for dd-trace-c |
| `handler.rs` | `init`/`init_from_env`/`bootstrap_complete`/`shutdown`, the `crash_handler` itself, fork/receiver/collector children, bounded reap, alt-stack install, loader-env scrubbing |
| `state.rs` | Static `Meta` (heapless strings), init-state machine, per-signal `ORIG_FN`/`ORIG_FLAGS`/`OWN_SIGNAL` atomics, runtime option atomics, `Stage` enum |
| `sys.rs` | Raw syscall layer: rustix + inline asm on Linux x86_64/aarch64 (`fork_raw` via `clone(SIGCHLD)`, `process_vm_readv`, `wait4`, …), libc fallback elsewhere, errno save/restore |
| `backtrace.rs` | Frame-pointer walk seeded from `ucontext`, probing frame records with `process_vm_readv` (`read_own_mem`) so corrupt frames return failure instead of faulting |

Plus:

- FFI surface: `libdd-crashtracker-ffi/src/collector_signal_safe.rs`
  (`ddog_crasht_signal_safe_init[_from_env]`, `_bootstrap_complete`, `_shutdown`,
  `_set_stage`, `_capabilities`, `_owned_signal_count`, `_owns_signal`).
- Feature re-plumbing: `libdd-crashtracker/Cargo.toml` gained a `std` feature; all
  std-only deps are optional; `collector_signal-safe` pulls only
  `heapless`, `libc`, `rustix`, `serde` (no-std), `serde-json-core`.
- Std collector integration: `libdd-crashtracker/src/collector/api.rs` now
  acquires a `signal_owner` guard so only one collector arms handlers.
- E2E tests: `libdd-crashtracker/tests/collector_signal_safe_e2e.rs` — Linux
  receiver round-trip through a shell receiver, and a portable degraded
  report-to-fd test.
- cbindgen currently *excludes* `libdd_crashtracker::collector_signal_safe`
  (`libdd-crashtracker-ffi/cbindgen.toml:74`) — headers are not yet generated.

### Restored support files (commit `6ae6136f3`, verified present and green)

- `libdd-crashtracker/src/signal_owner.rs` — `AtomicU8` owner slot;
  `acquire` is **reentrant for the same owner** (CAS-or-already-mine),
  `release` only clears when the caller owns it.
- `libdd-crashtracker/src/collector_signal_safe/capabilities.rs` — capability
  bits `RECEIVER_OK|PROC_VM_READV|FORK_OK|DEV_NULL|PIPE_OK|REPORT_FD_OK`,
  degradation bits with reason strings (`missing_receiver`,
  `no_process_vm_readv`, `no_fork`, `no_dev_null`, `no_pipe`, `pipe_failed`,
  `fork_failed`, `receiver_unavailable`, `report_to_fd`); `publish()` probes
  receiver executability, a `process_vm_readv` self-read, fork support,
  `/dev/null`, and pipe creation, and `store`s both atomics (safe for re-init).
- `.github/workflows/crashtracker-signal-safe.yml` — check (x86_64 + aarch64
  cross-check), clippy, nextest, symbol guard.
- `tools/check_signal_safe_symbols.sh` — bans undefined symbols
  (`malloc|free|pthread_mutex_lock|__rust_alloc|getenv|dlsym|getauxval|fork|posix_spawn|pthread_atfork|__libc_*`)
  in the no-default-features rlib via `nm -u`.

### What is broken right now (verified 2026-07-06)

Compile, unit tests (20), and e2e tests (4) are green with
`--no-default-features --features collector_signal-safe`. One failure:

**`tools/check_signal_safe_symbols.sh` fails: `U getenv` in the
libdd-crashtracker rlib.** Source: `collector_signal_safe/config.rs:311`
(`env_get` → `libc::getenv`), reachable only at init time
(`prepare_from_env`), never on the crash path — but the guard scans the whole
rlib's undefined symbols and cannot scope to the crash path. Fix in Phase 0.

### Validation commands (run after every phase)

```bash
cargo check -p libdd-crashtracker --features collector_signal-safe
cargo check -p libdd-crashtracker --no-default-features --features collector_signal-safe   # no-std-ish build must stay green
cargo check -p libdd-crashtracker-ffi --no-default-features --features collector_signal-safe
cargo build --workspace --exclude builder                                                  # default features unaffected
cargo +nightly-2026-02-08 fmt --all -- --check
cargo +stable clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run -p libdd-crashtracker --features collector_signal-safe
cargo nextest run --workspace --no-fail-fast
cargo test --doc
```

If `Cargo.lock` changes: `./scripts/update_license_3rdparty.sh && cargo deny check`
(the branch already added `heapless`, `rustix`, `serde-json-core` to
`LICENSE-3rdparty.csv`; keep it in sync). If FFI is touched: `cargo ffi-test`.
New files need Apache-2.0 headers (`./scripts/reformat_copyright.sh`).

---

## Phase 0 — Make the restored CI gate green (P0, do first)

The missing modules are back (see §0); the remaining P0 items are small.

### 0.1 Fix the symbol-guard failure: drop `libc::getenv`

`tools/check_signal_safe_symbols.sh` currently fails on `U getenv`
(`config.rs:311`, `env_get`).

Fix: replace `libc::getenv` with a direct walk of the global `environ` array
(`unsafe extern "C" { static mut environ: *mut *mut c_char; }` is already
declared in `handler.rs:30-32` — hoist it into `sys.rs` and share). Match
`NAME=` by byte prefix, return the value slice. This keeps env reads
init-time-only *and* keeps the strict guard honest: unlike `getenv`, the walk
is trivially auditable as allocation-free and lock-free.

Rejected alternative: removing `getenv` from the banned regex ("env reads are
init-only by construction"). Rejected because a future crash-path `getenv`
regression would then pass CI silently — the guard exists precisely to make
that error loud.

Add a unit test for the new lookup (set a var via `std::env::set_var` in a
`#[cfg(test)]` std context, read it back through the new function).

### 0.2 Guard-script robustness

`tools/check_signal_safe_symbols.sh` globs *all*
`liblibdd_crashtracker*.rlib` under `target/debug/deps`. A developer's dirty
target dir can contain stale rlibs from std-feature builds (false failures),
and conversely a stale no-std rlib could mask a regression (false pass —
`cargo build` may not rebuild if fingerprints match but old artifacts linger).
Fix: build into a dedicated target dir
(`CARGO_TARGET_DIR=target/signal-safe-guard`) inside the script, or parse the
freshly built artifact path from `cargo build --message-format=json`.

### 0.3 Housekeeping before the first real PR

- `cargo check -p libdd-crashtracker --features collector_signal-safe` (with
  default `std` also enabled) emits an unused-import warning for
  `access_executable`/`fork_supported` re-exports at `sys.rs:568-572`? —
  verified clean now that capabilities.rs uses both; re-check under
  `--all-features` clippy which CI runs with `-D warnings`.
- The branch history is `wip` commits + a merge. Rebase/squash into
  conventional commits before opening PRs (PR split at the end of this plan).
- `cargo nextest` is used by the restored workflow; it is not installed in the
  local dev environment — install it or keep using `cargo test` locally
  (nextest's process-per-test isolation matters here: the signal-safe tests
  mutate process-global signal state; `TEST_GLOBAL_LOCK` covers plain
  `cargo test`).

Contract notes for later phases (actual restored implementations):

- `signal_owner::acquire` is reentrant for the same owner. This means a second
  `init_result` call by the same collector does not fail at the owner gate —
  single-shot-ness is enforced only by `state::begin_init`. Keep this in mind
  for lifecycle work (1.4) and the interplay tests (5.2).
- Capability bit names: `RECEIVER_OK, PROC_VM_READV, FORK_OK, DEV_NULL,
  PIPE_OK, REPORT_FD_OK`; degradations include *both* init-time
  `DEGRADED_MISSING_RECEIVER` ("missing_receiver") and crash-time
  `DEGRADED_RECEIVER_UNAVAILABLE` ("receiver_unavailable") — use the former for
  probe failures, the latter for crash-time exec/exit failures (2.3).
- `capabilities::publish` `store`s (not `fetch_or`s) both atomics, so re-init
  (1.4) gets fresh state for free; only `note_degraded` accumulates.

---

## Phase 1 — Correctness and safety hardening (concrete findings in current code)

Each item below is a real issue found by review of the committed code; fix with
a test where possible.

### 1.1 `IN_APP_CHAIN` never resets after `siglongjmp` recovery (`handler.rs:540`)

If the app handler recovers via `siglongjmp`, control never returns to
`crash_handler`, so the function-local `static IN_APP_CHAIN` stays `true`
forever. Every subsequent crash then *skips* the app-first path and reports
immediately — silently breaking Mode A after the first recovered signal (which
is exactly the HotSpot/V8 hot case: recoverable SIGSEGVs happen constantly).

A plain boolean cannot express what the guard needs to know. On entry the
handler must distinguish three cases: a fresh signal delivery, true recursion
(a crash *inside* the app handler), and a stale flag left by a `siglongjmp`
that abandoned a previous handler frame. Clear-on-return misses the third
case; a sticky flag conflates it with the second.

Design (replace `IN_APP_CHAIN` with two atomics):

- Before invoking the app handler, store the handling thread's tid
  (`AtomicI32`) and an approximate stack position (`&local as usize`,
  `AtomicUsize`). Clear both on the normal-return path.
- On entry with a stored tid equal to our own, the previous invocation on this
  thread never returned — it either longjmp'd away or we are nested inside it.
  Disambiguate by stack position: if the current frame is *outside* the
  recorded frame (the stack has unwound past it), the old invocation is dead →
  reset the guard and run app-first normally. If the current frame is *inside*
  (deeper than) the recorded one, this is true recursion → skip app-first and
  go straight to reporting.
- On entry with a different thread's tid stored, skip app-first; the report
  path is already serialized by `COLLECTING`.

This is the one deliberately non-obvious mechanism in the module — per the
guiding principles it gets a doc comment explaining the three cases and the
stack-growth-direction assumption, plus both e2e tests from 4.1
(recover-then-crash-again must still run the app handler first;
crash-inside-app-handler must report once and not loop).

### 1.2 Mode A infinite loop when the app handler returns without recovering

Flow today: app handler returns normally, still installed → `app_recovered()`
returns true (`handler_after` is a function pointer, not `SIG_DFL`) → we return
without reporting. For a *synchronous* fault the kernel re-executes the faulting
instruction → our handler runs again → app-first again → infinite ping-pong.

dd-trace-c's semantics: "recovered" means the app handler transferred control
away (longjmp) or fixed the fault. A returning handler that leaves itself
installed and did not fix the fault will loop *regardless* of crashtracker —
but we amplify it by adding fork/report machinery never, since we skip
reporting. Mitigation:

- Add a per-signal repeat counter (same PC + same address within N entries →
  treat as unrecovered: report once, then chain with
  `RestoreDefaultAndRefault`). Cap cheap: two `AtomicUsize` (last_pc, count).
- Test: app handler that returns without fixing (install handler, deref null,
  handler just returns) — process must terminate with SIGSEGV and produce
  exactly one report, not hang.

### 1.3 Alt-stack is process-global but `sigaltstack` is per-thread (`handler.rs:250-261`)

`install_alt_stack_if_requested` installs the single static 64 KiB stack only on
the *calling* thread. Crashes on other threads run on their normal stack even
with `SA_ONSTACK` set (kernel ignores the flag if that thread has no alt stack),
and — worse — if two threads did share one alt stack and crashed concurrently,
they would corrupt each other. Actions:

- Document loudly on `create_alt_stack`/`use_alt_stack` (Rust doc + FFI header
  comment): "alt stack applies to the init thread only; stack-overflow crashes
  on other threads are collected on the faulting thread's stack".
- Keep dd-trace-c default (`false`/`false`) as-is.
- Optional follow-up (defer): expose a per-thread `ddog_crasht_signal_safe_arm_thread()`
  that installs a caller-provided alt stack on the current thread.
- Compare with the std collector's alt-stack handling in
  `collector/signal_handler_manager.rs` — reuse its sizing policy
  (`page_size`-aware, guard page) if we ever create per-thread stacks.

### 1.4 `shutdown()` → re-`init()` is permanently bricked (`state.rs:58-82`)

`INIT_STATE` moves `UNINIT → INITIALIZING → READY`; `begin_init` only succeeds
from `UNINIT`, and `shutdown()` never resets it. Also `fail_init` parks the
state at `FAILED` forever (e.g. a transient bad receiver path can never be
retried).

Chosen approach — allow re-init: `shutdown()` stores `INIT_UNINIT` after
releasing the signal owner; `fail_init()` stores `INIT_UNINIT` too (each
failure path in `init_result` already unwinds owner + state coherently).
Prerequisite audit of `Meta` publication: `prepare()` mutates the static `Meta`
via `meta_mut()`, which is only sound while no handler can run; the current
order (prepare → install handlers → `HANDLERS_ENABLED.store(true, Release)`)
is correct, and on re-init `begin_init`'s CAS keeps mutation exclusive. Keep
that order and add a comment that `meta_mut` may only be called between
`begin_init` and handler installation.

If the audit finds a soundness blocker, fall back (decision D2) to documented
one-shot semantics with a distinct `AlreadyInitialized` result — never a bare
`Failed`, so the caller can tell the difference.

Test: init → shutdown → init → crash e2e still produces a report.

### 1.5 `uninstall_crash_handler` drops the original `sa_mask` (`handler.rs:710-728`)

Only `sa_sigaction` and `sa_flags` are saved/restored; the displaced handler's
signal mask is discarded (restored with an empty mask). Save `old.sa_mask` at
install (needs a static array of `libc::sigset_t` — wrap in an
`UnsafeCell`+`Sync` holder like `AltStackStorage`, guarded by the same
init-exclusivity argument) and restore it on uninstall. Same for the app-chain
`invoke_handler` path: dd-trace-c applies the app handler's mask semantics; at
minimum document that we invoke the app handler with *our* mask in effect.

### 1.6 Capability/degradation state across re-init — already handled

`capabilities::publish` `store`s both atomics, wiping prior probe results and
`note_degraded` accumulation, and `state::clear_signal_state()` runs in
`install_all_handlers`. Nothing to do beyond a regression test once 1.4 lands
(re-init after a degraded crash must not carry stale degradation tags).

### 1.7 FFI entry points must not unwind (AGENTS.md rule)

`ddog_crasht_signal_safe_*` call into code designed not to panic, but the repo
rule is explicit: wrap each FFI body in `std::panic::catch_unwind` and map to
`SignalSafeInitResult::Failed` / no-op. Cheap, mechanical. (The FFI crate is
std; only the library crate is no-std-capable.)

### 1.8 `sanitize_clone` when `/dev/null` open fails (`handler.rs:209-218`)

If `open_readwrite` fails (containers with restricted /dev), the children keep
the app's stdin/stdout/stderr. For the *receiver* child this leaks the app's
stdio to an exec'd process; for the collector child it is harmless. The init
probe already reports this (`DEGRADED_NO_DEV_NULL` / `no_dev_null` tag), but
`sanitize_clone` ignores the `DEV_NULL` capability bit at crash time —
optionally close stdio outright in the receiver child when /dev/null is
unavailable (receiver must tolerate closed stdout/stderr — verify against
`receiver_entry_point_stdin`).

### 1.9 Emitter capacity truncation is silent

`SECTION_BUF_CAPACITY` is 4096 and `emit_json_section` returns `false` on
overflow, which aborts the whole remaining report (`&&` chains). A long
service/env or 64 frames × 20 bytes fits, but config JSON is capped at 2048 and
metadata with 20 × 288-byte tags can exceed 4096 → *entire report silently
truncated after config*. Actions:

- Compute worst-case section sizes from the heapless capacities and either
  raise `SECTION_BUF_CAPACITY` for the metadata section or split tag emission.
- On section-emit failure, still attempt to write `DD_CRASHTRACK_DONE` and the
  message section so the receiver processes a partial report instead of timing
  out; add degradation tag `report_degraded:truncated`.
- Unit test: metadata at max tag capacity round-trips; oversized section
  produces a partial-but-terminated stream.

### 1.10 Small items

- `handler.rs:19` `EXIT_CODE_FAILURE = 125` collides with common shell/xargs
  conventions; fine, but document.
- `config.rs:302-308` `cstr_bytes` uses `read_volatile` per byte — replace with
  a plain loop (volatile is unnecessary and slow); it also has no length cap:
  bound it (e.g. 4096) to avoid walking off an unterminated string from FFI.
- `config.rs:270-279` `set_str` silently truncates at capacity mid-char-loop —
  fine, but truncation of `service` should probably be observable
  (degradation bit or debug breadcrumb).
- `sys.rs` non-Linux `poll_sleep_ms` uses `libc::poll` with null fds — OK; add
  a comment that EINTR shortens the sleep and the reap loop tolerates it.
- `state.rs:13` `NSIG = 128` — index is the raw signal number; fine for Linux
  and BSDs. Add a compile-time assert or comment.
- `mod.rs` `SI_TKILL` fallback for non-Linux is `i32::MIN` — document why
  (macOS has no SI_TKILL; the sentinel must never match).

---

## Phase 2 — Missing-capability handling (beyond restore)

Goal: the collector must *always* do the best thing available and say what it
couldn't do. The restored `capabilities.rs` taxonomy is the foundation; this
phase extends it.

### 2.1 Probe results in the report and via FFI

- `ddog_crasht_signal_safe_capabilities()` already returns the bits; also add
  `ddog_crasht_signal_safe_degradations()` so integrators can log at init.
- Emit `capability_bits`/`degradation_bits` as additional tags
  (`capabilities:<hex>`) — cheap and makes fleet-wide triage possible. Keep the
  human-readable `report_degraded:<reason>` tags as the primary signal.

### 2.2 Seccomp sacrificial-child probe (deferred item from the notes, now scheduled)

`process_vm_readv` under seccomp can (a) return `EPERM` → handled gracefully
today, or (b) `SECCOMP_RET_KILL` → kills the *collector child* mid-crash and the
report loses its stack silently. Add an opt-in init probe:

- `SignalSafeInitConfig::probe_seccomp: bool` (default `false` — forking at init
  is a global effect; per repo conventions it must be opt-in).
- Probe: `fork_raw()`; child calls `read_own_mem` on itself and `_exit(0)`;
  parent waits ≤100 ms. Exit 0 → OK; killed by SIGSYS/SIGKILL → clear
  `PROC_VM_READV`, set `DEGRADED_NO_PROC_VM_READV`; timeout → kill + treat as
  unknown (keep capability, it's the status quo). Note the existing in-process
  probe in `capabilities::probe_process_vm_readv` only catches errno-returning
  denials (`EPERM`); the sacrificial child exists precisely for
  `SECCOMP_RET_KILL` policies.
- With the capability cleared, `emit_crash_report` already degrades to
  `seed_only` stackwalk (ip + lr only) — that path exists (`handler.rs:361-369`).

### 2.3 Receiver re-validation at crash time

`RECEIVER_OK` is probed once at init; the receiver binary can be deleted/moved
later (container image GC, tmp cleanup). At crash time the failure mode today
is: fork succeeds, `execv` fails, receiver child exits 125, collector writes
into a pipe nobody drains until the pipe buffer fills, then the parent reaps on
timeout. Improvements:

- In `collect_crash`, after reaping the receiver, check its exit status
  (`waitpid_nohang` currently discards status — return it) and if it exited 125
  and `report_fd` is set, re-emit the report to `report_fd` with
  `DEGRADED_RECEIVER_UNAVAILABLE|DEGRADED_REPORT_TO_FD`. This makes the fd
  fallback cover late disappearance, not just init-time absence.
- Order matters: reap receiver *before* deciding the crash path is done; today
  the collector is reaped first with a 500 ms budget, then the receiver with
  timeout+grace — keep that order (writer first), just capture status.

### 2.4 `close_fds_on_receiver` is plumbed but unimplemented

`CLOSE_FDS_ON_RECEIVER` is stored (`state.rs:105`) and configurable through FFI
but nothing reads it. Implement in `receiver_child` before `execv`:
`close_range(3, ~0u, 0)` via raw syscall (`SYS_close_range`, Linux 5.9+; on
ENOSYS fall back to nothing — do NOT iterate /proc/self/fd, that's not
fork-safe), after the report fd has been dup'd to stdin. Test: open a marker fd
with `O_CLOEXEC` unset in the parent, crash, assert the receiver does not see it
(receiver script checks `/proc/self/fd`).

### 2.5 Non-Linux degraded mode documentation

On macOS/other Unix, `fork_supported() == false` → report-to-fd only. Make this
an explicit, documented support matrix in `collector_signal_safe/mod.rs` docs
and the FFI header:

| Target | fork collection | stackwalk | fallback |
|---|---|---|---|
| Linux x86_64/aarch64 | clone(SIGCHLD) | fp + process_vm_readv | report_fd |
| other Linux arches | none (libc fallback module) | none | report_fd |
| macOS/iOS | none | none | report_fd (siginfo-only minimal report) |
| non-Unix | compile_error (lib.rs:54) | — | — |

macOS best-effort `fork()` mode is decision D3: stay degraded-fd-only, revisit
only if an integrator asks.

---

## Phase 3 — Safety options surface

The option set on `SignalSafeInitConfig` is good; this phase makes each option
sound, validated, and documented.

### 3.1 Validation & clamping (config.rs)

Central `fn validate(config) -> Result<Normalized, InitError>` instead of the
scattered `normalized_*` helpers, and make `init_result` return *why* it failed:

```rust
#[repr(i32)] pub enum InitResult {
    Enabled = 0,
    DisabledByConfig = 1,
    Failed = 2,               // keep for ABI compat
    AlreadyInitialized = 3,
    OwnerConflict = 4,        // std collector holds the signals
    InvalidConfig = 5,        // path too long, bad fd, etc.
}
```

(The FFI enum mirrors it; cbindgen headers regenerated in Phase 5.) Checks:

- `receiver_path` length < 512 (today `set_receiver_path` fails → generic
  `Failed`; surface as `InvalidConfig`).
- `report_fd`: if ≥ 0, verify with `fcntl(F_GETFD)` at init; warn-degrade if bad.
- `max_frames` clamp (exists), `collector_reap_ms`/`receiver_timeout_secs`
  bounds (exists) — move into `validate` and unit-test the boundaries.
- `use_alt_stack && !create_alt_stack`: allowed (caller made their own alt
  stack) — document. `create_alt_stack && !use_alt_stack` is pointless →
  `InvalidConfig`. (dd-trace-c silently pairs them; we reject instead —
  explicit is better than implicit, and a rejected config is visible at init
  while an auto-corrected one hides the integrator's misunderstanding.)

### 3.2 `disarm_on_entry` semantics (`handler.rs:523-525`)

Today it resets the *current* signal to SIG_DFL on entry, which is a
crash-loop-proofing option, but the chain logic at the bottom then consults
`effective_target` and may reinstall/raise accordingly. Interaction bugs:

- After disarm, `ChainAction::Resume` (app disposition SIG_IGN) leaves the
  signal at SIG_DFL, not restored to our handler nor to SIG_IGN — next
  occurrence terminates the process with no report and without honoring the
  app's IGN. Chosen behavior: with `disarm_on_entry`, after a non-genuine
  signal, restore the pre-entry disposition (our handler) before returning.
  Add tests for (disarm × {genuine, external-async, ignored}).

### 3.3 `block_signals` and `SA_NODEFER`

`BLOCK_SIGNALS` adds all crash signals to `sa_mask` (good default). Document
the interplay: when we invoke the app handler *from* our handler, the app runs
with our mask (all crash signals blocked) — a crash inside the app handler on a
*different* signal is deferred, not delivered, until we return; combined with
1.1's recursion guard this is the intended behavior. Add this to the module docs;
no code change.

### 3.4 Stack-overflow self-protection in the handler

The handler itself uses ~4–8 KiB of stack (`SECTION_BUF_CAPACITY` buffer is in
the *collector child*'s `emit_crash_report` frame, but `crash_debug`'s FdSink
and locals are small). On a SIGSEGV from stack overflow *without* alt stack, the
handler may fault immediately → kernel kills with default action (our handler
can't run: the signal is blocked during delivery and the second fault while
SIGSEGV is blocked force-terminates). That is acceptable and matches dd-trace-c,
but: verify `emit_crash_report`'s 4 KiB+ frame only ever exists in the forked
child or in the degraded direct-report path (parent handler frame!). Today
`direct_report` runs `emit_crash_report` **in the signal handler frame**
(`handler.rs:438-445`) — that's a ~5 KiB stack requirement in degraded mode.
Either document ("degraded fd reporting needs ~8 KiB of remaining stack") or
move the buffers to static storage (`UnsafeCell` scratch, safe because
`COLLECTING` guarantees single entry).

### 3.5 Timeouts

`RECEIVER_TIMEOUT_MS` is `secs*1000 + grace` capped only by u32 input; a huge
env-provided value stalls the crashing process for that long (the process is
dying anyway, but supervisors may SIGKILL and lose the report). Clamp to e.g.
60 s in `validate`. `poll_sleep_ms(100)` granularity is fine.

---

## Phase 4 — Test strategy

### 4.1 Keep and extend the two-binary e2e pattern

The self-exec pattern in `tests/collector_signal_safe_e2e.rs` (env-var-gated
"child" tests + orchestrating tests) is good; extend the matrix. Each scenario
is a child test fn + assertion block:

| Scenario | Child behavior | Assert |
|---|---|---|
| SIGSEGV genuine | deref null | report, `SEGV_MAPERR`, non-zero frames on x86_64/aarch64 |
| SIGFPE | integer div by zero | report emitted; si_code name policy (see 5.3) |
| External async signal | parent sends SIGSEGV via `kill` to child pid | **no** report; child terminates by default action |
| Self-sent async | child `raise(SIGSEGV)` | report (genuine per self-pid rule) |
| App handler recovers (Mode A) | install SIGSEGV handler with `siglongjmp`; crash; continue | no report; process exits 0 |
| Recover **then** genuine crash | as above, then deref null with handler removed | exactly one report (regression for 1.1) |
| App handler gives up | handler restores SIG_DFL and returns | report, then process dies with SIGSEGV (refault, not raise — check exit signal) |
| Mode B | `DD_CRASHTRACKING_ALWAYS_ON_TOP=true` + recovering handler | report *and* process exits 0 |
| App registered via `signal()` not `sigaction()` | flags without SA_SIGINFO | app invoked with 1-arg convention (covers `invoke_handler` transmute) |
| Stuck receiver | receiver script sleeps forever | crashing process terminates within timeout+grace+ε; receiver reaped (no zombie) |
| Receiver deleted post-init | unlink receiver after init | fd fallback report with both degradation tags (2.3) |
| Bootstrap-only | `DD_CRASHTRACKING_ONLY_BOOTSTRAP=true`, crash after `bootstrap_complete()` | no report |
| Stage tags | crash before/after `bootstrap_complete` | `stage:crashtracker_init` vs `stage:application` in tags & message |
| Low-fd collision | child sets report pipe up so read end lands on fd 0-2 | report intact (covers `sanitize_clone` relocation) |
| Re-init after shutdown (if 1.4 chosen) | init→shutdown→init→crash | one report |

Timeout-sensitive tests must use generous budgets (CI is slow); gate the stuck-
receiver test behind `#[cfg(target_os = "linux")]`.

### 4.2 Golden receiver round-trip (reuse the real parser)

Highest-value compat test: feed the signal-safe emitter's exact byte stream into
the *actual* std receiver parser.

- Dev-only test in `libdd-crashtracker/tests/` compiled with
  `--features collector_signal-safe,receiver` (nextest already builds
  per-test-binary features via the workspace run with `--all-features`; add an
  explicit CI invocation `cargo nextest run -p libdd-crashtracker --features
  "collector_signal-safe,receiver" signal_safe_golden`).
- Build a `Report`+`CrashContext` with fixed values → `emit_report` into a
  `SliceSink` → run through the receiver's stdin parsing entry
  (`receiver` module exposes the line-protocol parser; use
  `receiver_entry_point_stdin`-adjacent internals or spawn
  `crashtracker-receiver` binary with stdin = the bytes and a file endpoint).
- Assert the resulting `CrashInfo`: sig names, `PROCESSINFO` section accepted,
  tags including `stage:`/`report_degraded:` preserved, config JSON deserializes
  into `CrashtrackerConfiguration` (this pins `build_config_json` as a wire
  contract — `resolve_frames: EnabledWithSymbolsInReceiver`, timeout struct
  shape, signal list).
- Golden file: check in the emitted stream (`tests/fixtures/signal_safe_report.golden`)
  and diff against regenerated output, so accidental wire changes are loud.

### 4.3 Banned-symbol guard (exists — refine)

`tools/check_signal_safe_symbols.sh` + the `Symbol guard` step in
`.github/workflows/crashtracker-signal-safe.yml` already implement the
link-level check (`nm -u` over the no-default-features rlib + rustix rlibs,
banning `malloc|free|pthread_mutex_lock|__rust_alloc|getenv|dlsym|getauxval|fork|posix_spawn|pthread_atfork|__libc_*`).
Remaining work:

- Fix the current `U getenv` failure (Phase 0.1) — the guard is red today.
- Isolate the build (Phase 0.2, dedicated `CARGO_TARGET_DIR`) so stale
  artifacts can't produce false results in either direction.
- Known limitation to document in the script: it scans the *entire* rlib's
  undefined symbols, so it enforces "the whole no-std build is clean", not
  "the crash path is clean" — stricter than strictly needed (init-time code is
  held to crash-path standards), which is the right trade-off; any exception
  must be an explicit regex change reviewed in the script, not an attribute in
  code.
- Optional addition: `heapless`/`serde-json-core` rlibs are not scanned — add
  them to the `find` list (they are no-std by construction, so this is cheap
  insurance).

### 4.4 Unit-test gaps

- `chain_action` × `disarm_on_entry` matrix (3.2).
- `write_i32` boundary: `i32::MIN` (current code does `wrapping_neg` on i64 —
  correct, but assert it).
- `hex_addr(usize::MAX)`, `hex_addr(0)`.
- `build_config_json` exact-string golden (not just `contains`).
- `capabilities` probe unit tests (probe failure paths: missing receiver path
  → `DEGRADED_MISSING_RECEIVER`; `report_fd` propagation → `REPORT_FD_OK`).
- aarch64: the restored workflow only runs `cargo check --target
  aarch64-unknown-linux-gnu` — compilation coverage, no execution. Consider
  upgrading to actually *run* the unit tests under qemu (`cross test` or a
  `taiki-e/setup-cross-toolchain-action` job); the aarch64-specific code
  (`arch_seed` LR fallback, `fork_raw` via `svc 0`, syscall wrappers) is
  exactly the kind that passes `check` and fails at runtime.

---

## Phase 5 — Reuse & compatibility with the existing crashtracker / libdatadog

### 5.1 Share the wire-protocol constants

`DD_CRASHTRACK_BEGIN_*` markers are hardcoded as byte strings in
`collector_signal_safe/mod.rs` *and* exist in the std collector/receiver
(`shared/constants.rs` or `crash_info` — locate with
`grep -rn "DD_CRASHTRACK_BEGIN" libdd-crashtracker/src --include=*.rs`).
Extract a no-std-safe `pub(crate) mod protocol` (plain `&'static str`/`&[u8]`
consts, zero deps, compiled under both `std` and `collector_signal-safe`) and
use it from the std emitter, the signal-safe emitter, and the receiver parser.
One source of truth kills silent divergence; the golden test (4.2) enforces it.

### 5.2 Signal-owner interplay tests (both directions)

`collector/api.rs` now bails if the signal-safe collector owns signals, and
`handler.rs` bails in reverse. Add tests:

- std `init` after signal-safe `init` → error string mentions the conflict
  (test with both features enabled).
- signal-safe `init_result` after std `init` → `OwnerConflict` (new variant, 3.1).
- std `shutdown`/`disable` releasing ownership → signal-safe init then succeeds
  (verify `collector/api.rs` release path — the diff shows release on failure;
  confirm the success-path teardown releases too; if the std collector has no
  full uninstall, document that switching collectors requires process restart).

### 5.3 si_code / signal-name parity with the receiver

The receiver deserializes `si_code_human_readable` into its `SiCodes` enum.
Audit `rust_si_code_name` + `rust_signal_name` outputs against
`crash_info`'s enums (`grep -n "SEGV_MAPERR\|SiCodes" libdd-crashtracker/src/crash_info/`).
Two known items from the parity doc (§12):

- `SIGFPE` `FPE_*` codes: `SiCodes` has no FPE variants → signal-safe emitter
  currently returns `"<unknown>"` for FPE codes. Confirm the receiver's serde
  tolerates `"<unknown>"` (it must map to an UNKNOWN variant, not error). If it
  errors, emit `"UNKNOWN"` or whatever the receiver's fallback spelling is —
  pin with a golden test (SIGFPE e2e in 4.1).
- `"<unknown>"` for unrecognized si_codes generally — same verification.

### 5.4 FFI headers (cbindgen) and the FFI type duplication

- Remove the `libdd_crashtracker::collector_signal_safe` exclusion from
  `libdd-crashtracker-ffi/cbindgen.toml` **or** keep the exclusion and ensure
  all `#[repr(C)]` types live in the FFI crate only (current approach —
  `SignalSafeConfig`, `SignalSafeInitResult`, `SignalSafeStage` are FFI-crate
  types; the exclusion just stops cbindgen from chasing internal lib types.
  That's fine — verify generated headers actually contain the
  `ddog_crasht_signal_safe_*` functions and structs: build with the `cbindgen`
  feature and inspect the header).
- The `Stage`/`InitResult` enums are duplicated (lib + FFI) with manual mapping —
  keep (repo FFI convention), but add a unit test asserting variant-value
  equality so they can't drift.
- Add a C example under `examples/ffi/` exercising
  `ddog_crasht_signal_safe_init` + abort + receiver script, wired into
  `cargo ffi-test` (per AGENTS.md step 4).

### 5.5 Builder/release integration

`builder` crate feature flags gate release artifacts (`crashtracker` flag). The
`std` feature refactor changed `crate-type` from `["lib","staticlib"]` to
`["lib"]` in `libdd-crashtracker/Cargo.toml` — verify the builder and any
downstream consumer didn't rely on the staticlib (grep `builder/` for
`crashtracker` staticlib references; run `cargo run --bin release -- --out /tmp/out`
smoke). Whether `collector_signal-safe` becomes part of the released FFI
artifact now or stays opt-in is decision D1 (team call, blocks PR 8); if
released, add the feature to the builder's crashtracker flag set and
regenerate headers.

### 5.6 Don't regress the std path

The `std` feature refactor touches every consumer of `libdd-crashtracker`.
Verify:

- `cargo check` for every workspace crate depending on `libdd-crashtracker`
  (sidecar, profiling-ffi if it re-exports crashtracker, bin_tests):
  `cargo build --workspace --exclude builder` covers it — plus
  `cargo nextest run --workspace --all-features --exclude builder --exclude test_spawn_from_lib`.
- `bin_tests` crash-tracking integration tests still pass (they exercise the
  std collector end-to-end): `cargo nextest run -p bin_tests`.
- crashtracker unit-test features still work:
  `cargo nextest run -p libdd-crashtracker --features libdd-crashtracker/generate-unit-test-files`.

### 5.7 Env-read policy carve-out

Repo convention: no hidden env reads. `prepare_from_env`/`init_from_env` are the
sanctioned preload bootstrap exception. Document this explicitly in the module
docs and keep `init(&SignalSafeInitConfig)` 100% env-free (audit: it is today —
`prepare()` never calls `env_get`). Add a doc section to
`libdd-crashtracker/src/README.md` describing the two entry points and which
one language tracers should use (the explicit one).

---

## Phase 6 — Deferred parity work (tracked, not in this plan's execution scope)

From `crashtracker-work-we-need-to-do.md`; keep priorities but do them after
Phases 0–5:

1. `sigaction`/`signal` PLT interposition + virtualized per-signal state
   (§3 there) — prerequisite for full Mode A fidelity with late-registering
   runtimes. The current `ORIG_FN` snapshot only sees handlers displaced at
   install time.
2. Receiver path discovery beside the loaded `.so` (dladdr glue, arch-suffixed
   names) (§9).
3. Packaging: musl receiver sidecar, size guard, staticlib + link-level symbol
   guard CI (§16, 4.3 here).
4. Whole integration-test matrix port from dd-trace-c's
   `crashtracker_preload_test.go` (§18) — the 4.1 matrix covers the highest-value
   half already.

## Suggested PR sequence

1. **fix(crashtracker): make signal-safe symbol guard pass** — Phase 0
   (environ-based env lookup replacing `libc::getenv`, guard-script target-dir
   isolation). Rebase the `wip`+merge history into clean conventional commits.
2. **fix(crashtracker): signal-safe handler chaining hardening** — 1.1, 1.2, 1.5,
   1.10 + chain e2e tests from 4.1.
3. **feat(crashtracker): lifecycle re-init and richer InitResult** — 1.4, 1.6,
   1.7, 3.1, 5.2 tests.
4. **feat(crashtracker): capability probes and degraded-mode coverage** — 2.1–2.5,
   1.8, probe tests.
5. **test(crashtracker): golden receiver round-trip + symbol guard + e2e matrix**
   — 4.2, 4.3, remaining 4.1, 5.3.
6. **refactor(crashtracker): shared protocol constants** — 5.1 (pure refactor,
   golden test keeps it honest).
7. **feat(crashtracker-ffi): headers, C example, ffi-test wiring** — 5.4, 1.7 if
   not done in PR 3.
8. **build: builder/release + CI (aarch64, staticlib nm guard)** — 5.5, 4.4 CI.

Every PR: conventional-commit title, run the full validation list from §0, and
`./scripts/update_license_3rdparty.sh` if the lockfile moved.

## Key risks

1. The restored CI workflow's symbol guard is red (`U getenv`) — nothing lands
   until Phase 0.1; and `signal_owner::acquire`'s same-owner reentrancy means
   the owner gate alone does not enforce single init (only
   `state::begin_init` does) — don't weaken that CAS during lifecycle work.
2. Mode A guard redesign (1.1/1.2) is subtle; land it only with the
   recover-then-crash-again and handler-returns-without-fixing e2e tests.
3. The `std` feature refactor is the largest blast radius — Phase 5.6 checks are
   not optional.
4. `serde-json-core`/`heapless` in the crash path: allocation-free but capacity-
   bounded; 1.9's truncation semantics decide whether reports degrade loudly or
   vanish.
