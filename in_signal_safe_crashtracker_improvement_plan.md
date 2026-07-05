# Signal-Safe Crashtracker Improvement Plan

Status: draft plan, written 2026-07-05 on branch `signal_safe_crashtracker` (HEAD `84eb21715`).
Audience: an engineer or LLM agent picking this work up with no prior context.

Terminology used throughout (see Workstream H for the code-side rename):

- **app-first policy** — default on-crash policy: if the application installed a real
  handler, run it first; if it recovers, do not report; if it gave up (restored
  `SIG_DFL`), report and terminate. Safe for managed runtimes (HotSpot, V8, .NET,
  Python faulthandler) that use SIGSEGV non-fatally.
- **report-first policy** — opt-in (`DD_CRASHTRACKING_ALWAYS_ON_TOP=true`): report
  first, then chain to the application handler.

---

## 1. Context

Branch `signal_safe_crashtracker` added a signal-safe, `no_std`-style in-process crash
collector to `libdd-crashtracker`, targeting preload/injection deployments where the
signal handler, the forked collector child, and the receiver child before `execve` must
all stay async-signal-safe: no allocation, no locks, no stdio, no `Drop`-dependent
cleanup, no panics. `crashtracker-work-we-need-to-do.md` at the repo root holds the full
gap analysis this branch was cut from; this plan builds on it and does not repeat it.

Code added by commit `84eb21715`:

| File | Role |
|---|---|
| `libdd-crashtracker/src/collector_signal_safe/mod.rs` | Wire emitter (`DD_CRASHTRACK_*` sections), chain-policy pure functions, `Sink` trait |
| `libdd-crashtracker/src/collector_signal_safe/handler.rs` | Signal handler, fork of receiver+collector children, reap loop, install/uninstall lifecycle |
| `libdd-crashtracker/src/collector_signal_safe/sys.rs` | Raw inline-asm syscalls (x86_64/aarch64 Linux) + libc fallback for other Unix |
| `libdd-crashtracker/src/collector_signal_safe/backtrace.rs` | Frame-pointer walk seeded from `ucontext`, probing memory via `process_vm_readv` |
| `libdd-crashtracker/src/collector_signal_safe/config.rs` | Init config, env snapshot (`DD_*` vars), fixed config JSON contract |
| `libdd-crashtracker/src/collector_signal_safe/state.rs` | Static state: `Meta` strings (heapless), per-signal atomics, stage tracking |
| `libdd-crashtracker-ffi/src/collector_signal_safe.rs` | C ABI: `ddog_crasht_signal_safe_{init,init_from_env,bootstrap_complete,shutdown,set_stage}` |
| `libdd-crashtracker/tests/collector_signal_safe_e2e.rs` | One e2e test (abort → report through a shell-script receiver) |

Feature gate: `collector_signal-safe` on both crates. The module is only compiled with
`--no-default-features --features collector_signal-safe` because `lib.rs` has a
`compile_error!` when `std` and `collector_signal-safe` are both enabled
(`libdd-crashtracker/src/lib.rs:54-57`).

Goals of this plan, in priority order:

1. Fix verified defects (some are build-breaking).
2. **Eliminate libc fallbacks on the crash path — `libc::fork()` above all** (A5/A5a).
3. Handle missing OS/environment capabilities gracefully (seccomp, missing receiver, containers, non-Linux).
4. Add safety options (alt stack, disarm-on-entry, report-to-fd, tunable budgets).
5. Build the test/CI story (currently zero CI coverage).
6. Adopt `rustix` for the syscall layer — decided — under a strict no-`dlsym` constraint (§6).
7. Complete the policy machinery: sigaction/signal virtualization and a working app-first policy (§5).
8. Decouple naming, constants, and metadata from the originating C-tracer integration (§9, Workstream H).

---

## 2. Verified defects (fix first — Workstream A)

These were confirmed by building/inspecting on 2026-07-05. Each is independently fixable.

### A1. `--all-features` breaks the whole workspace validation (BUILD-BREAKING)

`cargo check -p libdd-crashtracker --all-features` fails today with:

```
error: The collector_signal-safe feature requires no_std; build with --no-default-features and without the std feature.
```

AGENTS.md's standard validation includes
`cargo nextest run --workspace --all-features --exclude builder --exclude test_spawn_from_lib`,
which enables both `std` and `collector_signal-safe` → the documented CI/validation flow
cannot pass. Cargo features must be additive; a `compile_error!` on a feature
combination violates that.

**Fix:** remove the `compile_error!` at `libdd-crashtracker/src/lib.rs:54-57` and make the
features coexist. The `collector_signal_safe` module is already `core`-only + `libc` +
`heapless` + `serde-json-core`; nothing in it needs `not(feature = "std")`. Change the
module gate at `lib.rs:67` to `#[cfg(all(unix, feature = "collector_signal-safe"))]`.
Guard against *both* collectors being armed at runtime (an atomic "a collector owns the
signals" flag consulted by both `init` paths), not at compile time. This also unlocks the
golden-wire test in §7 (emit with the signal-safe emitter, parse with the std receiver, in
one test binary).

### A2. Does not compile on aarch64-linux: `libc::SYS_poll` does not exist there

`sys.rs:385` uses `libc::SYS_poll` in `poll_sleep_ms`. The aarch64 Linux syscall table has
no `poll` — only `ppoll` (verified against libc 0.2.186 sources: aarch64 defines
`SYS_ppoll = 73` and no `SYS_poll`). The raw-asm module is compiled for
`target_arch = "aarch64"`, so an aarch64 build of the feature fails.

**Fix:** use `SYS_ppoll` with a `KernelTimespec` (the struct already exists in sys.rs for
`clock_gettime`), or `SYS_clock_nanosleep`. Add an aarch64 `cargo check` to CI (§7) so
this class of bug can't land again. The rustix adoption (§6) removes this bug class
structurally; fix it directly only if the rustix PR doesn't land first.

### A3. App-first "did the app handler recover?" check is a tautology — crashes are never reported when an app handler exists

`handler.rs:359-375`: after invoking the app handler first, the code checks
`app_recovered(target.fn_ptr)` — but `target.fn_ptr` is the pointer captured *before* the
call, which is by construction a real handler (`Disposition::Handler`), so
`app_recovered()` always returns true and the function always returns without reporting.

Consequence: with any app crash handler installed at crashtracker-init time, a genuine
fault that the app handler does **not** recover from (it just returns, or restores
`SIG_DFL` and returns) produces **no crash report**. Correct app-first semantics: after
the app-first call, re-read the *current* disposition; only skip reporting if the app
kept a non-default handler installed (recovery contract), report if it reset to
`SIG_DFL`.

**Fix:** after `invoke_handler` returns, re-query the live disposition with
`sigaction(sig, NULL, &cur)` (later: from the virtualized table, §5) and pass *that* to
`app_recovered`. Also note the `siglongjmp` hazard: if the app handler longjmps out, no
code after `invoke_handler` runs — which is the correct "recovered" outcome, and is why no
`Drop`-bearing state may be live across that call (there is none today; keep it that way
and add a comment stating the invariant).

Related smaller issues in the same block:
- `IN_APP_CHAIN` (handler.rs:364) is never reset on the not-recovered path. Today the
  process usually dies afterwards so it's latent, but combined with `ChainAction::Resume`
  paths it can wedge the app-first flow for a later signal. Reset it deterministically.
- The `should_run_app_first(false, ...)` call hardcodes `force_on_top = false` because the
  outer `if` already checked `FORCE_ON_TOP` — fine, but fold the check into one place so
  the pure function is actually exercised with real inputs.

### A4. `FdSink` EINTR retry is wrong on the libc fallback path

`sys.rs` raw path returns `-errno` from the syscall directly, and `FdSink::put`
(sys.rs:31) tests `n == -libc::EINTR as isize`. The **libc fallback** `write` returns
`-1` with the error in `errno` — so on macOS/other-Unix, EINTR is treated as a hard
failure and the report is truncated. **Fix:** normalize both backends to one error
convention (return `-errno`), or check `errno()` when `n == -1` in the fallback. Covered
automatically by adopting `rustix` (§6).

### A5. libc fallback `fork_raw` is `libc::fork()` — runs `pthread_atfork` handlers. BAN IT.

`sys.rs:476`. `fork()` itself is nominally async-signal-safe per POSIX, but glibc and
macOS libc run `pthread_atfork` handlers which may take locks and allocate — in a
crashed, possibly lock-holding process this deadlocks or corrupts. This is exactly what
the raw `clone(SIGCHLD)` on Linux avoids, and it defeats the entire point of the
signal-safe collector on any target that hits the fallback.

**Fix (hard policy, not a patch): `libc::fork()` must not exist anywhere in this module.**
Delete `fork_raw` from the fallback `mod raw` entirely. Fork-based collection is only
available where a raw, atfork-free process-creation primitive exists — today that is
Linux x86_64/aarch64 via raw `clone(SIGCHLD)`. On every other target, `collect_crash`
must take a no-fork path: report-to-fd synchronous emit (§4.4) if configured, else
breadcrumb + clean chaining. There is no supported raw-syscall fork on macOS (Apple's
syscall ABI is private and `pthread_atfork` bypass is unsupported), so macOS gets the
no-fork degraded mode by design, not by accident — the existing std collector remains
the macOS answer. See A5a for the full fallback policy this is part of.

### A5a. Shrink the libc fallback tier to an explicit allowlist

The whole `#[cfg(not(linux+x86_64/aarch64))] mod raw` fallback (sys.rs:445-537) is
currently a silent "port" to targets nobody has audited. Replace the open-ended fallback
with an explicit three-tier policy:

- **Tier 1 (full support, fork-based collection):** Linux x86_64/aarch64 — raw syscalls
  only (asm today, `rustix` linux_raw after §6). Zero libc on the crash path.
- **Tier 2 (degraded, no-fork):** other Unix (macOS first). Only libc functions on the
  POSIX async-signal-safe list AND free of hidden locks/allocation are allowed, from a
  written allowlist: `write`, `close`, `dup2`, `kill`, `_exit`, `clock_gettime`,
  `sigaction`, `sigemptyset`, `raise`, `poll`. Explicitly banned on any crash-reachable
  path: `fork`, `posix_spawn`, `execv`-after-libc-fork, `waitpid` loops that assume a
  child we can't create, `malloc`-adjacent anything, `getenv` (init-only), `dlsym`
  (init-only, and see §6 — the shipped artifact must have no `dlsym` reference at all).
  Collection on Tier 2 = seed-frame minimal report to a pre-opened fd.
- **Tier 3 (unsupported):** everything else — `compile_error!` naming the feature, so a
  new target is a conscious porting decision instead of an untested fallback.

Enforcement, not convention: the §7.5 symbol guard must fail on `fork`, `posix_spawn`,
`pthread_atfork`, and `dlsym` references in the crash-path objects, and CI builds Tier 2
(macOS or a cfg-emulated build) so the allowlist cfg boundaries stay honest.

### A6. Non-Linux `siginfo_pid` fallback misclassifies external kills as genuine faults

`handler.rs:437-440`: the fallback returns `sys::getpid()`, which makes
`is_genuine_fault(si_code == SI_USER, si_pid == self_pid)` true for **any** external
`kill -SEGV <pid>`. On macOS, external async signals would be reported as crashes —
the exact thing the filter exists to prevent. **Fix:** on non-Linux read `si_pid` from the
proper `siginfo_t` field (it exists on macOS/BSD), or return a sentinel that makes the
filter answer "not genuine".

### A7. `static mut META` + unsynchronized publication

`state.rs:43-51`: `meta_mut()` hands out `&'static mut` from a `static mut`; two threads
calling `init` concurrently is UB, and all atomics use `Relaxed`, so the handler-enable
flag store does not *publish* the `META` writes to other cores.
**Fix:** make init single-shot via an atomic state machine
(`Uninit -> Initializing -> Ready`, CAS to enter), reject concurrent/second init, and
store `HANDLERS_ENABLED`/`INSTALLED` with `Release` + load with `Acquire` in the handler
path so `META` writes happen-before any handler execution. This keeps the handler
lock-free (reads only after an `Acquire` load of a `Ready` flag).

### A8. Hygiene (small, do in the same PR as A1)

- Unused import warning when building only the signal-safe feature (`cargo check
  -p libdd-crashtracker --no-default-features --features collector_signal-safe` warns).
- `cvt()` in sys.rs:255-262 is a no-op (`if` and `else` both return `ret`) — delete or
  make it meaningful.
- Feature name `collector_signal-safe` mixes `_` and `-`. Before anything ships, rename to
  `collector-signal-safe` (or `signal-safe-collector`) on both crates. Breaking-change
  cost is zero right now; nonzero later.
- The e2e test's cfg (`not(feature = "std")`) must be updated when A1 lands (drop the
  `not(std)` condition).

---

## 3. Workstream B — Missing-capability handling and graceful degradation

Principle: **probe at init time (safe context), record results in atomics, degrade with
visibility at crash time (constrained context).** Never discover a missing capability for
the first time inside the signal handler, and never fail silently — a degraded report
must say *how* it is degraded.

### B1. Init-time capability probe

Add a `capabilities` module + `Capabilities` bitset (one `AtomicU32`), populated by
`init`/`init_from_env` before arming handlers:

| Capability | Probe | Degradation if absent |
|---|---|---|
| `RECEIVER_OK` | `access(receiver_path, X_OK)` (raw `faccessat`) | Don't fork a receiver at crash time; use report-to-fd mode if configured (§4), else skip collection, still chain correctly |
| `PROC_VM_READV` | Perform one `process_vm_readv` self-read at init (read a stack local) | Stack walk emits seed frames only (IP/LR from ucontext); tag `stackwalk_method:seed_only` |
| `FORK_OK` | Optional: probe `clone(SIGCHLD)`+`_exit(0)`+reap of a trivial child at init | If seccomp forbids fork: in-process minimal emit to pre-opened fd (§4) or nothing; record in tag. Always false on Tier 2 targets (A5a) |
| `DEV_NULL` | `openat(/dev/null)` once | Child stdio redirection skipped; report still emitted (chroot/minimal-container case) |
| `PIPE_OK` | trivially true if `pipe2` works at init | crash-time `pipe2` can still fail on `EMFILE` — handle by aborting collection cleanly and chaining |

Document explicitly (doc comment + `crashtracker-work-we-need-to-do.md` cross-ref): a
seccomp policy of `SECCOMP_RET_KILL` on `process_vm_readv` kills the collector child —
the init-time probe is precisely what detects this without losing the main process
(probe in a sacrificial child if `FORK_OK`; the gap-analysis doc §11 calls this the
"preflight probe"). First cut: probe in-process for `EPERM`-style failures,
sacrificial-child probe as a follow-up.

### B2. Degradation visibility on the wire

Extend `emit_additional_tags` (mod.rs:427) to append, beyond `stage:<stage>`:

- `stackwalk_method:<fp_pvr|seed_only|none>`
- `report_degraded:<reason>` when any capability was missing (comma-free single token;
  emit one tag per reason).
- Keep tag capacity math in mind: `MAX_TAGS = 12`, `TAG_CAPACITY = 288` (mod.rs:21-22) —
  raise `MAX_TAGS` as needed; it's compile-time cost only.

### B3. Crash-time failure paths that currently lose the report silently

- `collect_crash` (handler.rs:306): if `pipe` fails → returns with no breadcrumb. Add a
  `crash_debug` line for every bail-out (pipe fail, fork fail, receiver fail) so
  `DD_TRACE_LOG_LEVEL=debug` diagnoses field issues.
- `receiver_child` exec failure (`execv` returned): exits 125; the parent can't tell
  "receiver missing" from "receiver crashed". With `RECEIVER_OK` probed at init this
  becomes rare, but still log via `crash_debug` in the parent when the receiver's reap
  discovers exit-by-125.
- `reap_or_kill` (handler.rs:286): `waited < 0` (e.g. `ECHILD` if the app has a
  `SIGCHLD` reaper thread that stole the wait, or `SA_NOCLDWAIT` is set) returns
  immediately — the receiver may then be killed by the subsequent code path or outlive
  wrongly. Handle `-ECHILD` as "someone reaped it, treat as done", other negatives as
  errors + breadcrumb. Note `SA_NOCLDWAIT`/`SIG_IGN`-on-SIGCHLD as a documented
  limitation (children get auto-reaped; `wait4` returns `ECHILD` immediately — the
  bounded poll loop then must fall back to a fixed sleep before `SIGKILL`).

### B4. Missing-capability install cases

- `install_crash_handler` (handler.rs:472) silently no-ops when a signal already has a
  non-default handler (deliberate: only claim `SIG_DFL` signals). Record which signals
  were *not* claimed (`OWN_SIGNAL` already exists; also breadcrumb + expose a count from
  `init` return or an FFI query `ddog_crasht_signal_safe_owned_signals()`) so
  integrators can tell "installed but owns 0 signals" from "installed".
- `init_from_env` returning `bool` collapses "disabled by env" and "failed". Return a
  3-state enum through FFI (`Enabled`, `DisabledByConfig`, `Failed`) — C ABI stability is
  not required (repo convention: no C ABI backward-compat guarantees).

---

## 4. Workstream C — Safety options

New fields on `SignalSafeInitConfig` (and the FFI struct). Defaults preserve current
wire-contract behavior (everything off unless noted).

1. **Alternate signal stack** (`create_alt_stack`, `use_alt_stack` — mirrors the std
   collector's names). Without `SA_ONSTACK`, a stack-overflow SIGSEGV re-faults inside
   the handler prologue and the process dies with no report. Implement: `sigaltstack`
   with a statically reserved buffer (no mmap needed for the minimal case; static array
   of `SIGSTKSZ*2`), set `SA_ONSTACK` when enabled. Keep default off; the config JSON
   currently hardcodes `create_alt_stack:false` and must instead reflect the actual
   runtime choice — `build_config_json` (config.rs:47) takes these options instead of
   hardcoding.
2. **sa_mask policy**: currently `sigemptyset` (handler.rs:481) — another managed signal
   arriving on another thread mid-collection is only guarded by the `COLLECTING` latch.
   Option to block all managed crash signals in `sa_mask` during handling (default on —
   it is strictly safer and cheap).
3. **Disarm-on-entry (double-fault safety)**: option to reset the current signal to
   `SIG_DFL` *immediately on handler entry* (before collection), so a fault inside the
   handler itself terminates the process with the kernel's original context instead of
   recursing. Trade-off vs. app-first chaining (which needs the handler to stay resident
   for `Resume`) — document; default off under the app-first policy, consider default-on
   under report-first.
4. **Report-to-fd mode**: `report_fd: Option<i32>` — when the receiver can't be spawned
   (`FORK_OK`/`RECEIVER_OK` absent, or Tier 2 target per A5a) or by explicit choice, emit
   the report directly from the handler (no fork at all) into a caller-pre-opened fd
   (file or socket). This is the capability-degraded backstop for hardened seccomp
   containers **and the only collection mode on Tier 2**. The emitter already works
   against any `Sink`; this is mostly plumbing plus a documented invariant that the fd
   was opened `O_APPEND|O_CLOEXEC` at init.
5. **Tunable budgets**: `collector_reap_ms` (default 500), `receiver_timeout_secs`
   (default 5), `max_frames` (default 32, cap at a compile-time `BACKTRACE_LEVELS_MAX`).
   Currently constants at handler.rs:18-22.
6. **Receiver child fd hygiene option**: `close_range(3, ~0U, 0)` in the receiver child
   before `execv` (after the report fd is dup'ed to stdin) so app fds don't leak into the
   receiver. Raw syscall, Linux 5.9+; fall back to nothing if `ENOSYS`. Default on.
7. **Memory-ordering + single-shot init** — see A7; it belongs to this workstream's
   "make the state machine safe" umbrella.

---

## 5. Workstream D — sigaction/signal virtualization and completing the crash policies

Virtualization is the largest remaining gap *and* it blocks a correct app-first policy
(A3 fixes the tautology, but without interposition the crashtracker never learns about
handlers registered **after** init — `ORIG_FN` only holds install-time state, and a late
app registration simply displaces our handler entirely).

Phased approach:

1. **D1 — live re-query (no interposition):** A3's fix. The app-first path consults
   `sigaction(sig, NULL, ...)` at crash time to find the current app handler. Correct for
   the "app registered before us" case; still blind to "app displaced us later". Cheap,
   land with Workstream A.
2. **D2 — exported-symbol interposition (preload artifact only):** new feature
   `signal-interpose` on `libdd-crashtracker-ffi` that exports `sigaction` and `signal`
   symbols; resolve the real functions **at init, never in the signal path** — and per
   the no-`dlsym` constraint (§6), prefer direct `syscall(SYS_rt_sigaction)` for the
   pass-through so no `dlsym(RTLD_NEXT)` is needed at all; if a true libc pass-through
   proves necessary, isolate any one-time symbol lookup to init and keep it out of the
   shipped-symbol guard's crash-path objects. For managed signals, record the app's
   requested disposition into the existing per-signal atomics (`ORIG_FN`/`ORIG_FLAGS` —
   add `APP_SET: [AtomicBool; NSIG]`), answer `oldact` from virtual state, and do not let
   the kernel handler change. Own installs bypass the wrapper. Wrappers must be
   transparent when crashtracker is disabled / doesn't own the signal. Known, documented
   limitations: raw `rt_sigaction` syscalls and `dlopen`-resolved slots are not covered.
   LD_PRELOAD symbol interposition requires no crate; `plt-rs` (§6) is the fallback only
   if a non-preload deployment ever needs PLT patching — out of scope for the first cut.
3. **D3 — policy test matrix:** app gives up via `SIG_DFL` restore; app recovers via
   `siglongjmp`; report-first reports before recovery; registration via `sigaction` and
   via `signal`; `SA_NODEFER` handling; errno preservation across the app-first call; a
   recovered runtime signal must not consume the one-crash latch (`COLLECTING` must not
   be set by a recovered app-first pass — verify ordering in `crash_handler`: today the
   app-first block correctly runs before the `COLLECTING` swap, keep it that way under
   refactor).

Small item to fold into D1: `uninstall_crash_handler` restores the *original* handler —
after D2 exists, forced shutdown must restore the **virtualized app** handler (the app's
latest registration), not the install-time one.

---

## 6. Workstream E — Upstream crate reuse

Research done 2026-07-05 with `gh repo view`/`gh search repos` (stars / last push /
archived checked).

### Adopt — DECIDED

| Crate | Repo health | Use here |
|---|---|---|
| **rustix** (`linux_raw` backend) | bytecodealliance/rustix — 2032★, pushed 2026-06-15, active | Replace `sys.rs` raw asm wholesale: `write`, `close`, `dup3`, `fcntl(F_DUPFD)`, `pipe2`, `openat`, `wait4(WNOHANG)`, `kill`, `ppoll`/`clock_nanosleep`, `clock_gettime`, `getpid`, `gettid`, `faccessat`, `close_range` |
| **sadness-generator** (in EmbarkStudios/crash-handling) | 186★, pushed 2026-05-12, active | **dev-dependency** for e2e tests: generates real SIGSEGV (heap & stack-overflow), SIGBUS, SIGFPE, SIGILL, SIGABRT, SIGTRAP crashes. Zero production footprint |

**Decision (owner sign-off 2026-07-05): use rustix.** `no_std`, no-alloc, libc-free
syscalls on Linux (`linux_raw` backend); eliminates A2 (per-arch syscall table bugs) and
A4 (error-convention mismatch) as whole bug classes; audited, widely-deployed syscall
stubs instead of ours.

**Hard constraint discovered during evaluation: no `dlsym` in the shipped artifact.**
rustix compiles its `src/weak.rs` module even on the `linux_raw` backend (gated
`any(linux_raw, all(libc, not(windows/espidf/wasi)))` in `src/lib.rs`), and it declares
an extern `dlsym` used for runtime symbol probing; older auxv code paths also resolved
`getauxval` this way. An undefined `dlsym` reference — even weak — is unacceptable for
the preload artifact: on glibc < 2.34 it drags in `libdl` linkage and the gap-analysis
doc explicitly requires old-glibc-safe loading with no hard libdl symbols.

rustix adoption rules for the implementer:

- `default-features = false` (no `std`, no alloc), minimal API features only
  (`pipe`, `process`, `event`, `time`, `fs`, `stdio` — trim to what's actually called).
- Force the `linux_raw` backend (default on Linux when `use-libc` is not enabled).
- **Auxv without libc:** rustix needs auxv (vDSO discovery for `clock_gettime`,
  `AT_SECURE`, page size). Current rustix main reads it libc-free via the
  `PR_GET_AUXV` prctl (kernel ≥ 6.4) with a `/proc/self/auxv` fallback — no `getauxval`,
  no `dlsym`. Pin a rustix version with that behavior (verify at adoption time; upgrade
  rustix rather than re-introducing a libc/dlsym path). If a pinned older version only
  offers weak-`getauxval` resolution, do not use it — bump instead. As a last resort we
  can read auxv ourselves without libc (`/proc/self/auxv`, or walking the initial stack
  past `environ`) at init and feed rustix through its explicit-auxv mechanism where the
  version provides one.
- **Verify, don't trust:** the §7.5 symbol guard must assert the crash-path staticlib
  has zero references to `dlsym`, `getauxval`, and `__libc_*` on Tier 1. If any rustix
  API we call pulls in a `weak!`-gated path, drop that API and keep a local raw wrapper
  for that one call.
- Keep hand-rolled raw asm **only** for what rustix deliberately omits: raw
  `clone(SIGCHLD)` fork. Verify rustix coverage of `process_vm_readv` at implementation
  time; keep the local asm wrapper if absent. Everything else in `sys.rs::raw` is
  deleted.
- On Tier 2 targets (A5a) rustix falls back to its libc backend — thin wrappers around
  the same calls, acceptable there *only* for functions on the A5a allowlist; the banned
  list (`fork` above all) applies regardless of which layer would provide it.
- Rejected alternatives, for the record: **syscalls** (jasonwhite, 141★, active — thinner
  but keeps errno/typing work on us), **linux-raw-sys** (constants only), hand-rolled asm
  (the status quo this replaces).

### Keep (already used, both healthy)

- **heapless** — rust-embedded/heapless, 1990★, pushed 2026-07-03. Check whether 0.9 is
  out of rc and worth the bump; otherwise stay on 0.8.
- **serde-json-core** — rust-embedded-community, 195★, pushed 2025-11-18. Slower cadence
  but small and stable in scope. Keep; no action.

### Evaluate later (P2, do not adopt now)

- **framehop** (mstange/framehop — 113★, pushed 2026-04-17) and **unwinding**
  (nbdd0121/unwinding — 135★, pushed 2026-06-13): better-than-frame-pointer unwinding for
  FP-omitted builds. framehop is `no_std`-able and allocation-free at unwind time *if*
  unwind tables are prefetched at init. Substantial init-time complexity; only worth it
  if seed-frames-only reports prove too weak in practice. Track as an optional
  `resolve_frames` upgrade.
- **minidump-writer / minidumper / crash-handler** (rust-minidump org 505★ pushed
  2026-06-29; EmbarkStudios crash-handling 186★): a strategically different architecture
  (out-of-process minidump capture). Not compatible with the `DD_CRASHTRACK_*` wire
  contract; reference material only.
- **plt-rs** (ohchase/plt-rs — 40★, pushed 2026-06-22): PLT patching for the
  interposition workstream if a non-LD_PRELOAD deployment ever needs it. **redhook**
  (geofft/redhook) is stale (last push 2022-10) — do not use.
- **itoa** (dtolnay — 379★, active): could replace `write_i32`/`hex_addr`, but those are
  ~40 proven lines; a new dep + LICENSE-3rdparty churn isn't worth it. Skip.
- **signal-hook** (vorner — 854★, active): not usable inside a crash handler; relevant
  only as documentation of coexistence expectations (apps using signal-hook register
  through `sigaction` → covered by D2 virtualization).

### Repo mechanics for any dependency change

Every `Cargo.lock` change requires `./scripts/update_license_3rdparty.sh` and
`cargo deny check` (CI-guarded). New files need Apache-2.0 headers
(`./scripts/reformat_copyright.sh`).

---

## 7. Workstream F — Tests and CI

Current state: 15 unit tests + 1 e2e, and **zero CI coverage** (verified: no reference to
`collector_signal` anywhere in `.github/`), and the e2e cfg means even
`--all-features` runs skip it. Ordered work:

1. **CI job (blocks everything else being trustworthy):**
   - `cargo check -p libdd-crashtracker --no-default-features --features collector_signal-safe`
     for `x86_64-unknown-linux-gnu` **and** `--target aarch64-unknown-linux-gnu`
     (check-only cross-compile is enough to catch A2-class bugs; no qemu needed initially).
   - `cargo nextest run -p libdd-crashtracker --no-default-features --features collector_signal-safe`
     on x86_64 Linux (runs unit + e2e).
   - clippy + fmt on the same feature set (`-D warnings`).
   - A Tier 2 build (macOS runner or cfg-emulated) so the A5a allowlist boundaries compile.
   - After A1 (features coexist), the default workspace runs also compile the module,
     shrinking the special-casing to the nextest invocation.
2. **Unit-test isolation:** `handler::tests::lifecycle_can_install_and_shutdown`
   installs real SIGSEGV/SIGABRT handlers inside the shared test process — move
   install/uninstall lifecycle tests behind the fork-a-child pattern already used by the
   e2e (self-exec with an env marker), or into `bin_tests`. A stray SIGSEGV in another
   test thread while these run currently gets eaten/misrouted.
3. **E2e matrix** (extend `collector_signal_safe_e2e.rs`; use sadness-generator per §6):
   - each managed signal (SEGV, ABRT, BUS, ILL, FPE) produces a parseable report with the
     right `si_signo_human_readable`;
   - external `kill -SEGV` → **no** report; self-`raise` → report (genuine-fault filter);
   - app handler gives up (`SIG_DFL` restore) → report; app handler `siglongjmp`s → no
     report, process continues (app-first policy — after A3/D1);
   - `DD_CRASHTRACKING_ALWAYS_ON_TOP=true` → report emitted, then app handler runs
     (report-first policy);
   - stuck receiver (`sleep`-script) is SIGKILLed within budget; process still terminates
     with the original signal disposition;
   - crash with fds 0/1/2 closed pre-crash (pipe lands on low fds → relocation path in
     `sanitize_clone`);
   - receiver env: receiver script dumps `env` → assert `LD_PRELOAD`/`LD_AUDIT` absent,
     `DD_*` present;
   - stack-overflow crash once alt-stack (§4.1) lands;
   - default-disposition **re-fault** path: after report, kernel terminates with original
     `si_code`/address (check via `waitpid` status + core pattern in the harness).
4. **Golden wire round-trip:** feed a signal-safe-emitter report into the real std
   receiver parser (`libdd-crashtracker` `receiver` feature) and assert the resulting
   CrashInfo fields (service/env/tags/frames/si_code, `PROCESSINFO` spelling). Possible
   in one test binary after A1; until then in `bin_tests`.
5. **Hot-path hygiene guard:** build a `no_std` staticlib example with the feature,
   `nm`/`objdump` the archive in a test script and assert the crash-path object files
   reference none of: `malloc`, `free`, `pthread_mutex_lock`, `__rust_alloc`,
   panic/`core::fmt` machinery, `getenv`, **`dlsym`, `getauxval`, `fork`, `posix_spawn`,
   `pthread_atfork`** (A5a + §6 enforcement). This is the regression fence that keeps
   future edits — and future rustix upgrades — honest. (Script under `tools/` or
   `bin_tests`, Linux-only.)
6. **Config JSON as a wire contract:** existing `config_json_contains_receiver_contract`
   test becomes a full golden-string comparison once `build_config_json` takes options
   (§4.1), so accidental contract drift fails loudly.

---

## 8. Workstream G — FFI polish and packaging

- FFI init should return the 3-state result (§B4) and there should be a query for owned
  signals + capability bits (diagnostics for integrators).
- `cbindgen` output: verify the new header entries (the branch touched
  `libdd-crashtracker-ffi/cbindgen.toml`); add the new enum/result types.
- No `catch_unwind` needed in these FFI entry points *iff* the crate stays panic-free —
  enforce with `#![deny(clippy::panic, clippy::unwrap_used, clippy::expect_used)]` (or
  equivalent lint table) on the `collector_signal_safe` module, plus the §7.5 symbol
  guard. Repo rule is "FFI must not unwind across the boundary" — deny-by-construction
  satisfies it; document that in the module header.
- Packaging decisions — receiver artifact ownership, musl builds, builder feature flag
  (`crashtracker` already exists; decide whether signal-safe rides it or gets its own) —
  keep as an explicitly deferred decision section; nothing in this plan hard-blocks on
  it except the receiver-path-discovery item below.
- **Receiver path discovery:** sibling-of-.so lookup via `dladdr` needs C glue for weak
  linkage on old glibc (same no-hard-libdl constraint as §6). Scope it as its own PR;
  until then the baked default + env override (config.rs:17-20,110) is the contract.

---

## 9. Workstream H — Decouple naming and constants from the originating C-tracer integration

The module was ported from a specific C tracer's crashtracker and still carries that
origin in code, naming, and docs. This workstream makes the signal-safe collector a
neutral libdatadog component that any integrator (C tracer, injector, other language
preloads) parameterizes — while keeping wire compatibility for the existing consumer.

1. **Policy naming:** rename the "always on top" boolean plumbing and all comments/docs
   from the origin's mode letters to **app-first** / **report-first** (the terms used in
   this plan). Public config field stays `force_on_top` or becomes
   `policy: {AppFirst, ReportFirst}` — pick one and rename consistently across module,
   FFI, and cbindgen output.
2. **Metadata is integrator-provided, not hardcoded:** `emit_metadata` (mod.rs:387-425)
   hardcodes `library_name: "dd-trace-c"`, `family: "native"`, and a default service
   name equal to the origin library's name. Replace with fields on
   `SignalSafeInitConfig` (`library_name`, `library_version`, `family`,
   `default_service`), snapshotted at init into `Meta`. Ship a preset constructor that
   reproduces today's exact tag set so the existing consumer's wire output is
   byte-identical (golden test, §7.6).
3. **Version constant:** `TRACE_C_VERSION` / `option_env!("DD_TRACE_C_VERSION")`
   (config.rs:12-15) becomes `library_version` in the init config; the build-env
   injection moves to the integrator's build. `Report::trace_c_version` field renamed
   accordingly (it currently feeds `runtime_version`, `library_version`, and
   `injector_version` tags — keep that mapping in the preset).
4. **Env var names:** `DD_TRACE_C_CRASHTRACKER_PROCESS` and the origin-specific baked
   receiver default (config.rs:17-20) get neutral primary names (e.g.
   `DD_CRASHTRACKING_RECEIVER_PATH`), with the old names kept as documented,
   lower-priority aliases so existing deployments keep working. `prepare_from_env`
   reads new-name-first.
5. **Docs sweep:** module headers, `crashtracker-work-we-need-to-do.md` framing, and FFI
   doc comments describe behavior in terms of app-first/report-first and "the preload
   integrator", not the originating tracer. The gap-analysis doc stays as historical
   record; new docs must not require reading it.
6. **FFI symbol audit:** the `ddog_crasht_signal_safe_*` names are already neutral —
   keep. Anything origin-specific that leaks into cbindgen headers gets renamed in the
   same PR as (1)-(4) (single breaking change, allowed by repo ABI policy).

Acceptance for this workstream: `grep -ri "dd-trace-c\|trace_c\|mode a\|mode b"` over
`libdd-crashtracker*/src` returns only (a) the compatibility-preset constants and env
aliases with comments explaining them, and (b) nothing in identifiers or public API.

---

## 10. Suggested PR sequence

Each PR independently green (fmt, clippy `-D warnings`, nextest, license CSV if deps
changed). Conventional Commits (`feat(crashtracker): ...`, `fix(crashtracker): ...`).

1. **fix(crashtracker): make signal-safe feature additive + portability fixes** —
   A1, A2, A4, A6, A8, feature rename. Adds the CI job (§7.1) in the same PR so the
   fixes are locked in.
2. **refactor(crashtracker)!: ban libc fork and adopt rustix syscall layer** —
   A5 + A5a (three-tier policy, fallback deletion) together with §6 rustix adoption
   (they touch the same file; doing them as one PR avoids porting the fallback twice).
   Keeps raw `clone` (+ `process_vm_readv` if absent from rustix); deletes the rest of
   sys.rs asm and the entire open-ended libc fallback; adds the §7.5 symbol guard
   including the `dlsym`/`fork` bans; LICENSE-3rdparty update.
3. **fix(crashtracker): correct app-first recovery detection and handler state machine** —
   A3, A7, D1, IN_APP_CHAIN reset, `sa_mask` default (§4.2). Unit tests for the policy
   functions with live re-query fakes; e2e app-handler scenarios (§7.3).
4. **refactor(crashtracker)!: decouple signal-safe collector from origin C tracer** —
   Workstream H (§9). Golden wire test proves byte-identical output via the
   compatibility preset.
5. **feat(crashtracker): capability probing and degraded-report tags** — §3 (B1-B4).
6. **feat(crashtracker): safety options** — §4 (alt stack, disarm-on-entry,
   report-to-fd, budgets, close_range). Config JSON becomes option-driven, golden test.
7. **feat(crashtracker): sigaction/signal virtualization for preload** — D2 + shutdown
   restore semantics + D3 matrix.
8. **test(crashtracker): e2e matrix, golden wire round-trip, hot-path symbol guard
   completion** — remaining §7 items (can be split across PRs 3-7 where scenarios
   become testable).
9. **feat(crashtracker): receiver path discovery** — §8 last item (needs C glue design,
   same no-libdl constraint).

## 11. Acceptance criteria (definition of done for this plan)

- `cargo check -p libdd-crashtracker --all-features` and the full AGENTS.md validation
  suite pass unmodified.
- The feature compiles for `aarch64-unknown-linux-gnu` (CI-enforced).
- **No `libc::fork`, `posix_spawn`, or `pthread_atfork` reference anywhere in the
  module; no `dlsym`/`getauxval`/`__libc_*` reference in the Tier 1 crash-path
  staticlib — all enforced by the §7.5 symbol guard in CI.**
- `sys.rs` hand-written asm reduced to raw `clone(SIGCHLD)` (+ `process_vm_readv` if
  rustix lacks it); everything else through rustix `linux_raw`.
- A genuine crash with a pre-registered non-recovering app handler produces a report
  (regression test for A3).
- Every degraded report carries a `report_degraded`/`stackwalk_method` tag; no silent
  losses on: missing receiver, `process_vm_readv` EPERM, pipe/fork failure.
- E2e matrix of §7.3 green on x86_64 Linux CI.
- New deps reflected in `LICENSE-3rdparty.csv`; `cargo deny check` green.
- Workstream H grep criterion met: no origin-tracer names or mode letters in
  identifiers/public API; compatibility preset covered by a byte-identical golden test.
- `crashtracker-work-we-need-to-do.md` updated: items delivered here checked off,
  deferred items (packaging, framehop, sacrificial-child probe, PLT patching) marked as
  such.
