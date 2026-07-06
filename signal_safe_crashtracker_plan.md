# Signal-Safe Crashtracker — Consolidated Improvement Plan

Status date: 2026-07-06. Branch: `signal_safe_crashtracker` (HEAD `b59e44071`).

This document supersedes `signal_safe_collector_improvement_plan.md` (deleted from
the working tree; recoverable at `git show 79f8b2a0d:signal_safe_collector_improvement_plan.md`).
The dd-trace-c parity analysis remains in `crashtracker-work-we-need-to-do.md`;
this plan references it and does not repeat it.

It is written to be executed by another engineer/LLM with **no prior context**.
Every claim marked *(verified)* was checked against the working tree on the
status date. Section 0 lists what is already done — do not redo it.

---

## Guiding principles

Apply these to every change (they are also the review bar):

- **Errors never pass silently.** Every degraded path leaves a trace: a
  `report_degraded:*` tag, a distinct `InitResult` variant, or a
  `crash_debug` breadcrumb. A silently skipped handler install or a silently
  truncated string is a bug even when the behavior is otherwise correct.
- **Explicit is better than implicit.** Env reads happen only in the
  `*_from_env` entry points. Failure reasons are enum variants, not booleans.
  Interacting options are validated, not auto-corrected.
- **There should be one obvious way — and one source of truth.** Duplicated
  constants, duplicated invariants, and hand-rolled serializers that shadow a
  serde schema are where wire formats silently diverge. This plan's Phase 1
  exists to remove every such duplication that can be removed, and to pin the
  rest with drift tests.
- **Simple is better than complex.** The crash path stays a straight line:
  probe at init, one handler, two forked children, fixed buffers. Prefer
  deleting code to adding it. Anything genuinely non-obvious (the app-chain
  stack-position guard) carries a doc comment explaining why.

---

## 0. Ground truth (verified 2026-07-06)

### What exists

`libdd-crashtracker/src/collector_signal_safe/` behind cargo feature
`collector_signal-safe` (note the hyphen), coexisting with the std `collector`
feature via the shared `signal_owner.rs` arbiter:

| File | Lines | Contents |
|---|---|---|
| `mod.rs` | 978 | Wire emitter (`emit_report`, `Sink`/`SliceSink`), chain-policy pure functions, signal/si_code name tables, report data types, capacity constants, ~300 lines of tests |
| `handler.rs` | 926 | `init`/`init_from_env`/`bootstrap_complete`/`shutdown`, `InitResult`, the `crash_handler`, double-fork collect path, app-chain guard, repeat-fault detector, alt-stack, reap logic |
| `config.rs` | 522 | `SignalSafeInitConfig`, `validate`, `prepare[_from_env]_result`, hand-rolled config-JSON builder, env parsing, clamps |
| `sys.rs` | 728 | Raw syscall layer: rustix + inline asm on Linux x86_64/aarch64 (`fork_raw` via `clone(SIGCHLD)`, `process_vm_readv`, `wait4`, `close_range`), libc fallback elsewhere, `environ` walk (no `getenv`) |
| `state.rs` | 203 | Static `Meta` (heapless), init-state machine, per-signal orig-handler/mask atomics, runtime option atomics, `Stage` |
| `backtrace.rs` | 183 | Frame-pointer walk seeded from `ucontext`, probing frames with `process_vm_readv` so corrupt stacks fail instead of faulting |
| `capabilities.rs` | 143 | Capability bits (`RECEIVER_OK, PROC_VM_READV, FORK_OK, DEV_NULL, PIPE_OK, REPORT_FD_OK`), degradation bits + `DEGRADATION_REASONS` tag table, init-time `publish()` probes |

Plus: FFI surface `libdd-crashtracker-ffi/src/collector_signal_safe.rs`
(`ddog_crasht_signal_safe_*`, all `catch_unwind`-wrapped, `#[repr(C)]` enums
with lib↔FFI value-equality tests); shared `src/protocol.rs` (wire markers,
used by std emitter, signal-safe emitter, and receiver — `shared/constants.rs`
now re-exports it); e2e tests `tests/collector_signal_safe_e2e.rs`; receiver
round-trip test `receiver/mod.rs:112` feeding the signal-safe emitter's bytes
through the real receiver parser; CI workflow
`.github/workflows/crashtracker-signal-safe.yml`; symbol guard
`tools/check_signal_safe_symbols.sh`.

### Already done — do not redo

The previous plan's Phases 0–3 were largely implemented by commits `5da67273c`
and `b59e44071` *(all verified in the tree)*:

- `getenv` replaced by a direct `environ` walk (`sys.rs:614`); symbol guard
  **passes** (`bash tools/check_signal_safe_symbols.sh` → exit 0).
- App-chain recursion guard is tid + stack-position based
  (`handler.rs:37-38,180-209`), surviving `siglongjmp` recovery.
- Repeat-fault detector for app handlers that return without fixing the fault
  (`handler.rs:39-41,211-226`).
- `shutdown()`/`fail_init()` reset `INIT_STATE` to `UNINIT` → re-init works
  (`state.rs:89-95`).
- Original `sa_mask` saved and restored (`handler.rs:819,837`).
- `InitResult` has `DisabledByConfig/AlreadyInitialized/OwnerConflict/InvalidConfig`
  variants plus a central `validate()` (`config.rs:336`).
- `close_fds_on_receiver` implemented via raw `SYS_close_range`
  (`handler.rs:377-378`, `sys.rs:307`).
- Receiver exit-125 detection with re-emit to `report_fd`
  (`handler.rs:592`).
- Truncation degrades loudly: `emit_truncated_tail` always terminates the
  stream and tags `report_degraded:truncated` (`mod.rs:589`).
- Loader-env scrubbing (`LD_PRELOAD`/`LD_AUDIT`) before receiver exec
  (`handler.rs:345`).
- FFI `catch_unwind` on every entry point; support-matrix doc in `mod.rs:10-17`.

### What is red right now

Exactly one failure *(verified)*:

**`collector_signal_safe::config::tests::config_json_contains_receiver_contract`
fails on Linux.** The golden string at `config.rs:435-443` hardcodes
`"signals":[11,6,10,4,8]` — `10` is SIGBUS on BSD/macOS, but
`CRASH_SIGNALS` (`config.rs:40`) uses `libc::SIGBUS`, which is `7` on Linux.
The test encodes one platform's numbers; the code is portable. Fix in Phase 0.

Also *(verified)*: `tools/check_signal_safe_symbols.sh` is mode `100644` —
`./tools/...` fails with *Permission denied*; CI survives only because the
workflow says `bash tools/...` (`crashtracker-signal-safe.yml:46`).

### Validation commands (run after every phase)

```bash
cargo check -p libdd-crashtracker --features collector_signal-safe
cargo check -p libdd-crashtracker --no-default-features --features collector_signal-safe
cargo check -p libdd-crashtracker-ffi --no-default-features --features collector_signal-safe
cargo build --workspace --exclude builder
cargo +nightly-2026-02-08 fmt --all -- --check
cargo +stable clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run -p libdd-crashtracker --features collector_signal-safe   # or cargo test if nextest absent
cargo nextest run --workspace --no-fail-fast
cargo test --doc
bash tools/check_signal_safe_symbols.sh
```

If `Cargo.lock` changes: `./scripts/update_license_3rdparty.sh && cargo deny check`.
If FFI is touched: `cargo ffi-test`. New files need Apache-2.0 headers
(`./scripts/reformat_copyright.sh`). Signal-state-mutating tests must hold
`TEST_GLOBAL_LOCK` (`mod.rs:42`) — nextest's process-per-test isolation is why
CI uses it.

---

## Phase 0 — Green branch (P0, do first)

### 0.1 Fix the platform-dependent golden test

Replace the hardcoded signal numbers in
`config_json_contains_receiver_contract` (`config.rs:429-443`) with an
expectation built from `CRASH_SIGNALS` itself (format the array the same way
`build_config_json` does), or assert the full string with the signal segment
spliced in. The golden test's job is to pin the *shape* of the receiver
contract; the numeric values are `libc`'s to define per-platform. Keep one
additional assertion that `CRASH_SIGNALS` contains SIGSEGV/SIGABRT/SIGBUS/
SIGILL/SIGFPE by symbolic name so the set itself is still pinned.

### 0.2 Housekeeping

- `chmod +x tools/check_signal_safe_symbols.sh` (`git update-index --chmod=+x`).
- Branch history contains `wip` commits and merges — rebase/squash into
  conventional commits before opening PRs (sequence at the end).
- `cargo nextest` is not installed in the local dev environment; either
  install it or use `cargo test` locally (the `TEST_GLOBAL_LOCK` makes plain
  `cargo test` safe).

---

## Phase 1 — DRY and simplification

This is the headline phase. Each item removes a duplication or shrinks the
code; none changes wire behavior (the receiver round-trip test at
`receiver/mod.rs:112` and the config golden test are the guardrails — run them
after every step).

### 1.1 Split `mod.rs` (978 lines) into cohesive submodules

`mod.rs` currently mixes four concerns. Split, keeping `mod.rs` as the façade
with the same `pub use` surface (zero API change):

| New file | Moves from `mod.rs` |
|---|---|
| `emitter.rs` | `Sink`, `SliceSink`, `emit_report[_with_metadata]`, `emit_minimal_report`, `emit_json_section`, `emit_config/metadata/additional_tags/kind/stacktrace/message/truncated_tail/done`, `put_marker_line`, capacity constants, `push_tag` |
| `policy.rs` | `Disposition`, `ChainAction`, `SignalContext`, `disposition_of`, `app_handler_is_real`, `should_run_app_first`, `app_recovered`, `is_genuine_fault`, `chain_action` |
| `signal_names.rs` | `rust_signal_name`, `rust_si_code_name`, `signal_specific_si_code_name`, `signal_has_address`, the `SI_*`/`SEGV_*`/`BUS_*`/`ILL_*`/`FPE_*` consts |
| `report.rs` | `Metadata`, `SignalInfo`, `ProcInfo`, `Frame`, `Report`, `CrashContext`, `Tag`/`Tags` aliases |

Move each block's tests with it. Pure mechanical refactor; do it in one commit
with no logic changes so review is trivial.

### 1.2 Replace the hand-rolled config-JSON builder with a serde struct

`build_config_json` (`config.rs:123-164`) is 40 lines of `push_str`/`write!`
chains hand-assembling JSON that must match `CrashtrackerConfiguration`'s
serde schema — the classic shadow-serializer. The module already depends on
`serde` + `serde-json-core` and already serializes every report section that
way (`emit_json_section`, `mod.rs:389`). Replace with:

```rust
#[derive(Serialize)]
struct WireConfig<'a> {
    additional_files: [&'a str; 0],
    create_alt_stack: bool,
    use_alt_stack: bool,
    demangle_names: bool,
    endpoint: Option<()>,          // serializes as null
    resolve_frames: &'a str,       // "EnabledWithSymbolsInReceiver"
    signals: &'a [i32],
    timeout: WireTimeout,          // { secs: u32, nanos: u32 }
    unix_socket_path: Option<()>,
}
```

serialized with `serde_json_core::to_slice` into the existing
`CONFIG_JSON_BUF_SIZE` buffer (plus the trailing `\n`). This deletes the
manual escaping/ordering logic entirely; field order in the struct pins the
byte-exact output, and the receiver round-trip test proves
`CrashtrackerConfiguration` still deserializes it. Keep the (now
platform-correct, per 0.1) golden test as the wire contract.

*Rejected alternative*: reusing `CrashtrackerConfiguration` itself — it is
std-only (`Vec`, `Endpoint`) and its serde output is the *target*, not a tool
usable in a no-std crate. The drift risk is covered by the round-trip test.

### 1.3 One source of truth for wire/receiver-shared constants

Three small duplications *(verified)* to collapse into the shared layer:

- **Crash-signal list.** `config::CRASH_SIGNALS = [SIGSEGV, SIGABRT, SIGBUS,
  SIGILL, SIGFPE]` (`config.rs:40`) vs legacy `DEFAULT_SIGNALS = [SIGBUS,
  SIGABRT, SIGSEGV, SIGILL]` (`collector/api.rs:17-22`) — the legacy set omits
  SIGFPE. These are semantically different (fixed set vs configurable
  default), so don't force one list; instead move both to a tiny shared
  `crate::shared::signals` (or extend `protocol.rs`) as named consts with a
  comment stating the intended difference, and add a test asserting the
  signal-safe set is a superset of the legacy default. If the difference is
  *not* intended, that surfaces immediately (decision D2 below).
- **Default receiver timeout.** `DD_CRASHTRACK_DEFAULT_TIMEOUT = 5000 ms`
  (`shared/constants.rs:8`) vs `RECEIVER_TIMEOUT_SECS = 5` (`config.rs:33`).
  Derive the latter from the former.
- **`#![allow(dead_code)]` at `protocol.rs:4`** — with three consumers
  (std emitter, signal-safe emitter, receiver) most constants are live; scope
  the allow to the actually-unused items or remove it so dead protocol
  constants become visible again.

### 1.4 si_code / signal-name drift-proofing

`signal_names.rs` (post-1.1) deliberately reimplements the receiver's
`SignalNames`/`SiCodes` tables allocation-free — that duplication stays (the
legacy path uses `num_derive` + a C shim, unusable in-handler). Pin it instead
of merging it: add a `#[cfg(all(test, feature = "receiver"))]` test that, for
every signal in `CRASH_SIGNALS` × every named si_code, feeds
`rust_signal_name`/`rust_si_code_name` output through the receiver's serde
deserialization of `SigInfo` and asserts it produces the corresponding enum
variant (and that the `"<unknown>"` fallback maps to the receiver's UNKNOWN
handling rather than an error). Wire the combined-features invocation into the
CI workflow. This turns silent table drift into a red test.

### 1.5 Crash-path micro-simplifications

- **`emit_stacktrace` allocates a fresh `[0u8; 4096]` per frame inside its
  loop** (`mod.rs:564`, up to 64 iterations). Hoist the buffer out of the
  loop. A frame line needs ~40 bytes; also consider a dedicated small buffer
  (e.g. 256 B) — the 64 KiB alt stack is the budget, spend it deliberately.
- **Consolidate the hand-rolled formatters** — `hex_addr` (`mod.rs:442`),
  `hex_u32` (`mod.rs:547`), `write_i32` (`handler.rs:244`) — into one
  `fmt.rs` with unit tests (`i32::MIN`, `usize::MAX`, `0`). `hex_u32` is the
  only `core::fmt` user on the crash path; rewriting it in the same style as
  `hex_addr` makes the crash path fmt-free, which also shrinks the code the
  symbol guard has to trust.
- **`cstr_bytes` in the FFI** (`collector_signal_safe.rs:191-201`) and
  `set_str` truncation (`config.rs:300-309`): truncating `service` or
  `receiver_path` silently is an errors-pass-silently violation. Receiver-path
  truncation already returns `InvalidConfig` *(verified)*; make metadata
  truncation observable with a `crash_debug` breadcrumb or an init-time
  degradation note.

### 1.6 Alt-stack: one documented policy

Two divergent implementations *(verified)*: legacy mmaps
`max(SIGSTKSZ, 16 × page)` plus a `PROT_NONE` guard page
(`collector/signal_handler_manager.rs:139-173`); signal-safe uses a static
64 KiB array with no guard page (`handler.rs:23-29,332-343`). The static
approach is right for the no-std path (no mmap at init required), but:

- Document both choices side by side in a comment in each file referencing
  the other ("static because no-std/no-mmap; no guard page — overflow past the
  alt stack corrupts adjacent statics rather than faulting; acceptable because
  the process is already crashing").
- Optional improvement (small, worth it): place the alt stack in its own
  page-aligned static and `mprotect` its first page `PROT_NONE` *at init time*
  (init may syscall freely). That restores guard-page semantics without any
  crash-path cost. Fold the decision into D3.

### 1.7 Uniform capability gating

`collect_crash` checks `report_fd >= 0` directly (`handler.rs:519`) while
`REPORT_FD_OK` exists as a capability bit; `DEV_NULL` is probed and published
but never gated on *(verified)*. Make the handler consult capabilities
uniformly: gate the fd fallback on `REPORT_FD_OK`, and have `sanitize_clone`
branch on `DEV_NULL` (it already has the no-devnull path,
`close_stdio_without_devnull`). One idiom for one concept — and the
capability bits reported via FFI then truthfully describe what the handler
will actually do.

---

## Phase 2 — Missing capabilities and degraded modes

### 2.1 Silent handler-install skip must become visible

`install_crash_handler` returns silently when a signal's disposition is not
`SIG_DFL` (`handler.rs:805-806`) — an app that installed a SIGSEGV handler
before us means we never collect for that signal, observable only by polling
`owned_signal_count`. Add a degradation bit + reason
(`DEGRADED_HANDLER_PRESENT` / `report_degraded:app_handler_present:<sig>` — or
one bit plus a per-signal mask exposed through a new
`ddog_crasht_signal_safe_unowned_signals()` FFI getter). Init still succeeds
(partial coverage is better than none) but the fact is now on the record at
init time and in any report produced by the signals we do own.

Note: full Mode-A fidelity for *late*-registering runtimes needs the
sigaction/PLT virtualization deferred to Phase 6 — this item only makes the
install-time case loud.

### 2.2 Seccomp sacrificial-child probe (opt-in)

`probe_process_vm_readv` (`capabilities.rs:105`) catches errno-returning
denials (`EPERM`) but not `SECCOMP_RET_KILL` policies, which would kill the
collector child mid-crash and silently lose the stack. Add
`SignalSafeInitConfig::probe_seccomp: bool` (default `false` — forking at init
is a global effect and must be opt-in per repo conventions):
`fork_raw()`; child calls `read_own_mem` on itself and `_exit(0)`; parent
waits ≤100 ms. Killed by signal → clear `PROC_VM_READV`, set
`DEGRADED_NO_PROC_VM_READV`; timeout → kill, keep the capability (status
quo). The `seed_only` stackwalk fallback already exists downstream.

### 2.3 Make the one-shot `COLLECTING` semantics explicit

`COLLECTING` is set once and never reset (`handler.rs:663-667`): exactly one
collection per process lifetime, by design (a second concurrent crash must not
re-enter, and a second sequential crash is almost always the same fault). Keep
the behavior, but (a) doc-comment it at the static, (b) reset it in
`shutdown()` so the re-init lifecycle is coherent, and (c) add an e2e assert
that a second crash after a first report chains straight to default
disposition without forking.

### 2.4 Config surface gaps — deliberate scope decision

Currently not configurable *(verified)*: the signal set, alt-stack size,
`endpoint`/`unix_socket_path` (hardcoded null → receiver decides), and
stackwalk mode (always `EnabledWithSymbolsInReceiver`). Recommendation:
**add none of them now.** Each is a knob without a requesting integrator, and
the config JSON's hardcoded nulls are what keep the receiver contract simple.
Record this as decided (table below, D4); revisit per-signal opt-out first if
a runtime conflict report arrives (e.g. a JVM that must own SIGFPE — today
that case is handled by the app-handler-present skip from 2.1).

### 2.5 Non-Linux coverage

macOS/other-Unix code paths (`fork_supported() == false` → minimal report to
`report_fd`) compile but never run in CI *(verified: only ubuntu runners)*.
Add a `macos-latest` job running
`cargo test -p libdd-crashtracker --features collector_signal-safe` — the
degraded-fd e2e test is already portable (`collector_signal_safe_e2e.rs:93`).
This is cheap and pins the entire libc-fallback half of `sys.rs`, which today
is check-only dead weight that could regress freely.

---

## Phase 3 — Safety-option hardening

### 3.1 `disarm_on_entry` interaction matrix

`DISARM_ON_ENTRY` resets the signal to `SIG_DFL` on entry (`handler.rs:606`),
then the tail chain logic reinstalls/raises per `chain_action`. Unit-test the
matrix (disarm × {genuine fault, external async → `Resume`, ignored
disposition}) and fix the known gap: after disarm + `Resume`, the pre-entry
disposition (our handler) must be restored before returning, else the next
occurrence terminates with no report.

### 3.2 Stack-budget audit

Document the crash-path stack requirement next to `ALT_STACK_SIZE`
(`handler.rs:23`): handler frame + `collect_crash` locals +
(degraded path only) `emit_crash_report`'s section buffer ≈ 8–12 KiB, vs the
64 KiB alt stack. `direct_report` runs the emitter **in the signal-handler
frame** — with 1.5's per-frame-buffer hoist this is one 4 KiB buffer, fine,
but write the number down and add a `const _: () = assert!(...)` relating
`SECTION_BUF_CAPACITY` to `ALT_STACK_SIZE` so a future capacity bump can't
silently outgrow the stack.

### 3.3 Symbol guard hardening

The guard *(currently green, verified)* scans `nm -u` of the
no-default-features rlibs for
`malloc|free|pthread_mutex_lock|__rust_alloc|getenv|dlsym|getauxval|fork|posix_spawn|pthread_atfork|__libc_*`
(`tools/check_signal_safe_symbols.sh:25`). Extend:

- Add `calloc|realloc|posix_memalign|mmap|pthread_mutex_unlock|pthread_cond_[a-z]+|syslog|abort` —
  note `abort` will require auditing that nothing on the crash path links it
  (panics are already banned by clippy config).
- Build into a dedicated `CARGO_TARGET_DIR=target/signal-safe-guard` inside
  the script so stale dev artifacts can't produce false results in either
  direction.
- Known limitation, document in the script header: it scans the whole rlib,
  so init-time code is held to crash-path standards. That is the right
  trade-off; exceptions must be explicit regex changes reviewed in the
  script, never attributes in code.
- Longer term (with 5.1's release wiring): run `nm -u` on the released
  staticlib/cdylib too — rlib scanning can miss symbols introduced at final
  link.

---

## Phase 4 — Test strategy

### 4.1 E2E matrix (extend `tests/collector_signal_safe_e2e.rs`)

The self-exec pattern (env-gated child test fns + orchestrating asserts) is
good. Current coverage is exactly two scenarios *(verified)*: SIGABRT through
a shell receiver, and degraded report-to-fd. Highest-value additions, in
order:

| Scenario | Child behavior | Assert |
|---|---|---|
| **SIGSEGV with real stackwalk** | deref null | report, `SEGV_MAPERR`, >1 frame, `stackwalk_method:fp_pvr` — this is the only test that exercises `arch_seed`/`walk_fp`/`process_vm_readv` for real |
| App handler recovers (Mode A) | `siglongjmp` handler; crash; continue | no report; exit 0 |
| Recover, then genuine crash | as above, then handler removed, crash | exactly one report (pins the app-chain guard) |
| App handler gives up | handler restores `SIG_DFL`, returns | one report; process dies by SIGSEGV re-fault (check termination signal) |
| Mode B (`force_on_top`) | recovering handler + `DD_CRASHTRACKING_ALWAYS_ON_TOP=true` | report **and** exit 0 |
| External async signal | parent `kill -SEGV` child | **no** report; default termination |
| Self-sent async | child `raise(SIGSEGV)` | report |
| Stuck receiver | receiver script sleeps forever | process terminates within timeout+grace+ε; no zombie |
| Receiver deleted post-init | unlink receiver after init | fd fallback with `receiver_unavailable` + `report_to_fd` tags |
| Bootstrap-only | `DD_CRASHTRACKING_ONLY_BOOTSTRAP=true`, crash after `bootstrap_complete()` | no report |
| Stage tags | crash before vs after `bootstrap_complete` | `stage:crashtracker_init` vs `stage:application` |
| Loader-env scrub | init under `LD_PRELOAD=<dummy.so>` | receiver env lacks `LD_PRELOAD`/`LD_AUDIT` (receiver script dumps env) |
| fd hygiene | parent opens marker fd without `O_CLOEXEC` | receiver's `/proc/self/fd` lacks it (pins `close_range`) |
| SIGFPE | integer div-by-zero | report; si_code string accepted by receiver (with 1.4's drift test) |
| Re-init lifecycle | init → shutdown → init → crash | one report (pins 2.3's `COLLECTING` reset) |

Gate Linux-only scenarios `#[cfg(target_os = "linux")]`; use generous
timeouts (CI is slow).

### 4.2 Plug into `bin_tests` (the big reuse win)

The `bin_tests` harness is producer-agnostic: it asserts on the `CrashInfo`
JSON that the receiver writes to a `file://` endpoint
(`bin_tests/src/test_runner.rs:121-160`). Add:

- a new bin artifact (alongside `crashtracker_bin_test`) that calls
  `ddog_crasht_signal_safe_init` (through the FFI, so the C surface is what's
  tested) and crashes per a mode argument;
- a `TestMode`/validator asserting the signal-safe-specific fields: `stage:*`,
  `stackwalk_method:*`, `report_degraded:*` tags, and frames non-empty for
  SIGSEGV;
- reuse `test_crashtracker_receiver` unchanged — same protocol, same receiver
  *(already proven by the `receiver/mod.rs:112` round-trip test)*.

This buys the full validation pipeline (real receiver binary, real JSON
schema, telemetry file checks) for free and is the compatibility proof that
the two collectors remain interchangeable in front of one receiver.

### 4.3 Golden wire fixture

Check in the emitted byte stream for a fixed `Report`/`CrashContext`
(`tests/fixtures/signal_safe_report.golden`) and diff against regenerated
output in a test. The receiver round-trip test proves *parseability*; the
golden file makes any wire change *visible in review*. Regeneration path:
a `#[ignore]`d test that rewrites the fixture when run explicitly.

### 4.4 CI upgrades (`.github/workflows/crashtracker-signal-safe.yml`)

- aarch64 is check-only today *(verified)* — the inline-asm syscall stubs and
  `arch_seed` LR fallback are exactly the code that passes `check` and fails
  at runtime. Add an execution job (`cross test` or
  `taiki-e/setup-cross-toolchain-action` + qemu).
- macOS job (2.5).
- Combined-features run for the drift/round-trip tests:
  `cargo nextest run -p libdd-crashtracker --features "collector_signal-safe,receiver"`.
- Optional but cheap: an ASan job for the e2e tests (fork + raw syscalls +
  signal handlers is ASan's home turf; it will not understand the crash
  itself, so scope it to the lifecycle/init tests).

---

## Phase 5 — Reuse and compatibility with the rest of libdatadog

### 5.1 Ship it: release wiring (currently a dead end, verified)

The feature exists end-to-end in the crashtracker crates but **cannot reach
the released artifact**: `libdd-profiling-ffi/Cargo.toml` has passthroughs for
`crashtracker-collector`/`crashtracker-receiver` but none for
`collector_signal-safe`, and the builder's feature string
(`builder/src/profiling.rs:135`) doesn't mention it either. To ship:

1. Add `crashtracker-collector-signal-safe = ["libdd-crashtracker-ffi/collector_signal-safe"]`
   passthrough in `libdd-profiling-ffi/Cargo.toml`.
2. Add it to the builder's crashtracker feature set in
   `builder/src/profiling.rs` (gated on decision D1 below).
3. Remove the `libdd_crashtracker::collector_signal_safe` exclusion from
   `libdd-crashtracker-ffi/cbindgen.toml:75` **only if** cbindgen needs to
   chase lib types; the current design keeps all `#[repr(C)]` types in the
   FFI crate, so instead just verify the generated `crashtracker.h` contains
   the `ddog_crasht_signal_safe_*` functions and structs (build with the
   `cbindgen` feature and inspect).
4. Add a C example under `examples/ffi/` exercising init + abort + a receiver
   script, wired into `cargo ffi-test` (AGENTS.md step 4).
5. Smoke: `cargo run --bin release -- --out /tmp/out` and `nm -u` the
   produced staticlib for the banned-symbol list (closes the 3.3 gap).

### 5.2 Signal-owner interplay tests (both directions)

`signal_owner.rs` arbitration exists *(verified)*; test it explicitly with
both features enabled: std `init` after signal-safe `init` → error naming the
conflict; signal-safe `init_result` after std `init` → `OwnerConflict`; std
teardown releasing ownership → signal-safe init then succeeds (if the std
collector has no full uninstall, document that switching collectors requires
a process restart). Remember `signal_owner::acquire` is reentrant for the
same owner — single-init is enforced by `state::begin_init`'s CAS, not the
owner gate; don't weaken that CAS.

### 5.3 Explicit non-reuse decisions (recorded so nobody relitigates)

Surveyed the workspace for reuse; these are **deliberate no's** — the DRY
principle applies to sources of truth, not to forcing shared code across the
std/no-std boundary:

| Candidate | Verdict | Why |
|---|---|---|
| `libdd-capabilities`(-impl) | No | Trait-DI abstractions for HTTP/sleep/spawn (wasm portability), unrelated to runtime capability probing despite the name |
| `libdd-alloc::LinearAllocator` | No (for now) | Genuinely signal-safe bump allocator, but the collector's buffers are fixed-size by design; heapless capacities *are* the report size contract. Revisit only if variable-size scratch becomes necessary |
| `spawn_worker` | No | Trampoline-binary process spawner; allocates, memfd/tempfiles — not signal-safe, wrong tool |
| `libdd_common::unix_utils` (`alt_fork`, `PreparedExecve`…) | No in-handler | std/nix-based; the legacy collector's tools. `sys.rs` is the signal-safe equivalent and must stay independent |
| `blazesym`/symbolication | No in-collector | Symbolication stays in the receiver (`EnabledWithSymbolsInReceiver`); collector emits raw IPs only |
| Receiver, `CrashInfo` model, telemetry/errors-intake uploaders, `protocol.rs` | **Yes — already reused** | Collector-agnostic; the whole design hinges on one receiver parsing both collectors |

### 5.4 Don't regress the std path

The `std`-feature re-plumbing in `libdd-crashtracker/Cargo.toml` touches every
consumer. Non-optional checks: `cargo build --workspace --exclude builder`;
`cargo nextest run --workspace --all-features --exclude builder --exclude test_spawn_from_lib`;
`cargo nextest run -p bin_tests` (std collector e2e);
`cargo nextest run -p libdd-crashtracker --features generate-unit-test-files`
(per AGENTS.md). Verify no downstream crate relied on the removed
`staticlib` crate-type (grep `builder/` before assuming).

---

## Phase 6 — Deferred parity work (tracked, out of execution scope)

From `crashtracker-work-we-need-to-do.md`, in priority order; do after
Phases 0–5:

1. `sigaction`/`signal` PLT interposition + virtualized per-signal state
   (§3 there) — prerequisite for full Mode-A fidelity with late-registering
   runtimes; today's `ORIG_FN` snapshot sees only install-time handlers.
2. Receiver path discovery beside the loaded `.so` (dladdr glue,
   arch-suffixed names) (§9).
3. Packaging: musl receiver sidecar, size guard (§16).
4. Remainder of the dd-trace-c `crashtracker_preload_test.go` matrix (§18) —
   Phase 4.1 covers the highest-value half.

---

## Open decisions

Everything else in this plan is decided; these need a human call before the
affected PR:

| # | Decision | Recommendation | Blocks |
|---|---|---|---|
| D1 | Ship `collector_signal-safe` in the released FFI artifact now, or keep opt-in? | Team/product call | PR 7 (5.1 steps 2–5) |
| D2 | Legacy default signal set omits SIGFPE — intended divergence or historical accident? | Keep the divergence, document it (changing legacy defaults is a behavior change for existing SDKs) | PR 2 (1.3) |
| D3 | Add init-time `mprotect` guard page to the static alt stack? | Yes — init-time-only cost, real overflow protection | PR 3 (1.6) |
| D4 | Expose per-signal opt-out / alt-stack size / endpoint config now? | No — no requesting integrator; 2.1 covers the conflict case | nothing now |

---

## Suggested PR sequence

Each PR: conventional-commit title, full validation list from §0,
`./scripts/update_license_3rdparty.sh` if the lockfile moves.

1. **fix(crashtracker): platform-correct config golden + script exec bit** —
   Phase 0. Rebase the `wip`/merge history into clean commits here.
2. **refactor(crashtracker): split signal-safe module and share constants** —
   1.1, 1.3, 1.5, 1.7 (pure refactors; receiver round-trip + golden tests
   prove no wire change).
3. **refactor(crashtracker): serde wire-config + alt-stack guard page** —
   1.2, 1.6, 3.2.
4. **feat(crashtracker): visible degradations and capability polish** — 2.1,
   2.2, 2.3, 3.1, 1.4 drift test.
5. **test(crashtracker): e2e matrix, golden fixture, bin_tests integration** —
   4.1, 4.2, 4.3, 5.2.
6. **ci(crashtracker): aarch64 execution, macOS job, guard hardening** — 4.4,
   3.3, 2.5.
7. **build(crashtracker): release wiring, headers, C example** — 5.1
   (D1-gated), 5.4 verification.

## Key risks

1. **The wire format is the crown jewel.** Every Phase 1 refactor must run
   the receiver round-trip test and (once it exists) the golden fixture diff
   in the same commit. A wire regression that reaches a release breaks crash
   reporting for every SDK pinned to that version.
2. **aarch64 is untested at runtime** until 4.4 lands — the raw-syscall and
   `arch_seed` code there is the most likely place for a silent, shipped bug.
3. **The std-feature re-plumbing has the widest blast radius** — 5.4's checks
   are not optional on any PR that touches `Cargo.toml`.
4. **Refactors and behavior changes must not share a commit** (1.1's split is
   safe exactly because it moves code verbatim; hold that line in review).
