# Crashtracker Work We Need To Do

## Scope

This compares current libdatadog `libdd-crashtracker` against the crashtracking implementation in `/Users/pawel.chojnacki/work/dd-trace-c/crashtracker`.

Sources inspected:

- dd-trace-c branch `crashtracker` at `608537b3`.
- dd-trace-c commit `114f4a08`, which ports the in-process collector from `libdd_autoinstrument/crashtracker_linux.c` to `crashtracker_native`.
- dd-trace-c original C crashtracker from `origin/main:libdd_autoinstrument/crashtracker_linux.c`.
- libdatadog current `origin/main` at `d7b2aad37`.

Important framing: dd-trace-c already uses libdatadog for the receiver sidecar (`crashtracker_rust` is a tiny `libdd_crashtracker::receiver_entry_point_stdin()` wrapper). Most of the missing work is the in-process/preload-grade collector, signal policy, initialization, and packaging behavior.

## What libdatadog already has

Do not rebuild these unless needed to support the missing in-process work:

- Receiver-side stdin protocol parsing and crash upload.
- Telemetry and errors-intake payload generation.
- Configurable receiver process and Unix socket receiver modes.
- Panic hook and unhandled-exception reporting.
- Linux crashing-thread unwinding with bundled `libdd-libunwind-sys`.
- Linux all-thread collection in the receiver via `/proc/<pid>/task`, `PTRACE_SEIZE`, `PTRACE_INTERRUPT`, and remote libunwind.
- `PR_SET_PTRACER` scoping and sidecar peer PID verification for all-thread collection.
- macOS crashing-thread frame-pointer collection.
- Existing crash protocol sections for counters, spans, traces, additional tags, `/proc/self/maps`, ucontext, runtime stack callbacks, and whole-stack unhandled exceptions.
- Receiver compatibility with dd-trace-c's `PROCESSINFO` wire section. This needs golden coverage, not new parser work.
- Errno preservation in the signal handler.

## Main gap

libdatadog has a feature-rich crashtracker, but its current Unix collector is not yet equivalent to dd-trace-c's preload-grade implementation.

The dd-trace-c implementation assumes the signal handler and forked children are constrained environments. After a `fork`/raw `clone` from a signal handler in a potentially multi-threaded process, the child must be treated as async-signal-safe until `execve` or `_exit`. For the collector child there is no `execve`, so the entire collector child path must stay async-signal-safe. For the receiver child, every setup step before `execve` must stay async-signal-safe.

Current libdatadog still has `std`, `nix`, `UnixStream`, `File`, `serde_json`, `Box`-backed globals, formatting, and RAII/drop patterns reachable from the signal-handler/fork-child path. Some individual syscalls underneath are safe, but the Rust wrappers and destructors are not enough of a contract for the dd-trace-c preload use case.

## Progress on the signal-safe collector branch

Status as of 2026-07-06:

- Implemented a separate `collector_signal-safe` path that can coexist with the standard collector feature, with a runtime signal-owner guard so only one collector arms crash signal handlers.
- Added single-shot signal-safe initialization, release/acquire publication for handler enablement, fixed metadata snapshots, and configurable integrator metadata. The C-tracer defaults are now a compatibility preset rather than hardcoded report fields.
- Fixed the app-first recovery check by re-reading the live handler after the app handler returns, and kept the app-handler call path free of `Drop`-dependent state.
- Added init-time capability probes and report degradation tags for missing receiver, missing `process_vm_readv`, no fork support, `/dev/null`, pipe availability, and report-to-fd fallback.
- Replaced broad non-Linux forking with an explicit degraded no-fork policy. Linux x86_64/aarch64 keeps raw `clone(SIGCHLD)` for fork-based collection; other Unix targets can emit a minimal report to a pre-opened fd.
- Adopted `rustix` for ordinary fd/process/time wrappers where it fits, kept raw asm only where needed, normalized fallback errno handling, and removed libc `fork()` from the fallback path.
- Added optional alt-stack, signal-mask, disarm-on-entry, report-fd, timeout, max-frame, and receiver-fd cleanup config fields through Rust and FFI.
- Added Linux receiver e2e coverage, portable report-to-fd degraded e2e coverage, an aarch64 Linux check, and a symbol guard for banned crash-path symbols.

Deferred follow-up work:

- Full `sigaction`/`signal` virtualization and PLT interposition for late app/runtime handler registration.
- Receiver path discovery beside the loaded integration library, including architecture-suffixed receiver names.
- Sacrificial-child probing for seccomp policies that kill `process_vm_readv`.
- `close_range`/fd sweep behavior in the receiver child.
- Regenerating and reviewing cbindgen headers for the expanded FFI structs.
- Porting the complete dd-trace-c preload integration matrix, including app-handler recovery by `siglongjmp`, report-first policy tests, stuck receiver timeout tests, and receiver recursion prevention tests.
- Packaging decisions for the sidecar receiver and any preload-owned release artifacts.

## P0 work

### 1. Add a signal-safe in-process collector surface

dd-trace-c has `crashtracker_native`, a `no_std`, no-allocator staticlib linked into `libdd_autoinstrument.so`. It uses fixed-capacity buffers, `heapless`, `serde-json-core`, raw syscalls or `rustix` linux-raw calls, `panic = abort`, and a panic handler that exits immediately.

libdatadog needs an equivalent path or crate for Unix preload use:

- No allocation in the signal handler.
- No allocation in the forked collector child.
- No allocation in the receiver child before `execve`.
- No mutexes, stdio, lazy init, `pthread_once`, or logging paths that take locks.
- No `Drop`-bearing values live across calls to app signal handlers, because those handlers may recover with `siglongjmp`.
- No `std::io::Write` trait objects or `serde_json` in the crash path.
- No panics or unwinding through signal-handler or fork-child frames.
- Raw syscall wrappers for `clone`/`fork`, `_exit`, `write`, `close`, `dup2`, `fcntl`, `pipe`, `poll`, `waitpid`, `kill`, `getpid`, `gettid`, `clock_gettime`, and stack-memory probes.

Concrete libdatadog areas to audit or replace:

- `libdd-crashtracker/src/collector/crash_handler.rs`
- `libdd-crashtracker/src/collector/collector_manager.rs`
- `libdd-crashtracker/src/collector/receiver_manager.rs`
- `libdd-crashtracker/src/collector/emitters.rs`
- `libdd-common/src/unix_utils/fork.rs`
- `libdd-common/src/unix_utils/process.rs`
- `libdd-common/src/unix_utils/execve.rs`

Concrete current violations to remove first:

- `eprintln!` is reachable from signal or fork-child paths and is not async-signal-safe: `signal_handler_manager.rs` in `chain_signal_handler`, `collector_manager.rs` in `run_collector_child`, and `process_handle.rs` in `finish`. Replace with a fixed-buffer raw `write(2)` path or remove from crash paths.
- `libdd-common/src/unix_utils/fork.rs::is_being_traced()` uses `std::fs::File`, `Read`, UTF-8 parsing, and `Drop`-closed fds from `alt_fork()`, which is reachable from `Collector::spawn` and `Receiver::spawn_from_config` on the signal path. The underlying syscalls are not the problem; the std/RAII surface is.
- `collector/api.rs::mark_preload_logger_collector()` is a best-effort preload test hook using `dlsym` from the signal path on Linux. The preload design needs to remove it from the hot path, make it signal-safe, or explicitly scope it away from production crash handling.

### 2. Treat forked children as async-signal-safe

This is not optional. The dd-trace-c implementation deliberately treats both children as constrained:

- Receiver child: reset crash handlers, preserve/relocate the report fd if it is `0`, `1`, or `2`, redirect stdio to `/dev/null`, strip loader env, then `execv`.
- Collector child: reset crash handlers, preserve/relocate the write fd, redirect stdio, collect stack frames, emit the protocol stream, close the fd, then `_exit`.
- Parent: close both pipe ends, wait with bounded budgets, kill only after timeout, then reap.

Work needed in libdatadog:

- Remove `UnixStream::from_raw_fd`, `File`, `OwnedFd`, `PreparedExecve::exec()` wrappers, and destructor-dependent cleanup from the signal/fork-child path.
- Add explicit fd relocation before stdio redirection.
- Add child stdio redirection to `/dev/null` without clobbering the crash pipe/socket.
- Reset managed crash signals to `SIG_DFL` in both children so a crash in the child does not recurse into crashtracker.
- Fix the current concrete child gaps:
  - `collector_manager.rs::run_collector_child()` closes `0`, `1`, and `2` before writing to `uds_fd`; if `uds_fd` is one of those fds, the crash socket is destroyed.
  - `receiver_manager.rs::run_receiver_child()` uses naive `dup2` setup without collision handling for low-numbered report fds.
  - `collector_manager.rs::run_collector_child()` does not reset the managed crash signals to `SIG_DFL`; a collector fault while unwinding can re-enter the inherited crash handler.
- Use `_exit`, not Rust process exit or panic paths.
- Replace busy wait/reap behavior with the dd-trace-c bounded wait loop: collector around 500 ms, receiver timeout plus grace, then `SIGKILL` and reap.
- Treat libdatadog's current `ProcessHandle::finish()` semantics as different, not automatically broken: it waits for `POLLHUP`, then `SIGKILL`s and reaps. Matching dd-trace-c's wait-for-exit and kill-only-after-timeout behavior is a robustness/parity upgrade.

### 3. Add sigaction/signal virtualization

dd-trace-c installs its handler only on crash signals still set to `SIG_DFL`. It then PLT-interposes libc `sigaction` and `signal` so late app/runtime registrations cannot displace the crashtracker handler. The wrappers record the app's requested disposition in per-signal atomics and answer `oldact` from that virtual state.

libdatadog currently installs handlers and stores the previous handlers, but it does not keep itself on top if an app registers a handler later. That misses dd-trace-c's core preload behavior.

Work needed:

- Add a way to interpose or otherwise mediate `sigaction` and `signal` for owned crash signals.
- Track per-signal state:
  - handler pointer (`SIG_DFL`, `SIG_IGN`, or function pointer)
  - `SA_SIGINFO` flags
  - whether the app has set a handler
  - original handler displaced at install
  - whether crashtracker owns the kernel handler for this signal
- Virtualize `oldact` for app calls.
- Ensure crashtracker's own sigaction calls bypass the wrapper and hit the real libc function.
- Keep wrappers transparent when crashtracker is disabled or does not own the signal.
- Preserve dd-trace-c limitations explicitly: raw `rt_sigaction` syscalls and later `dlopen` PLT slots are not covered.

### 4. Port Mode A and Mode B crash policy

dd-trace-c has two on-crash policies:

- Mode A, default: managed-runtime-safe. If the app installed a real handler, run it first. If it recovers, do not report. If it restores `SIG_DFL`, report and terminate.
- Mode B, `DD_CRASHTRACKING_ALWAYS_ON_TOP=true`: report first, then chain to the app/runtime handler.

This matters for HotSpot, V8/Node, .NET, sanitizers, Python faulthandler, and other runtimes that use `SIGSEGV` non-fatally for null checks, safepoints, write barriers, or recovery.

Work needed:

- Add policy state and configuration.
- Land this with, or after, `sigaction`/`signal` virtualization. Mode A needs the virtualized effective app disposition to know whether there is a real app handler to call.
- Make the app-first call safe with respect to `siglongjmp`. This is the return-twice hazard: the app handler may jump through crashtracker frames, so no `Drop`-bearing guard or cleanup-required state can be live across the app-first call.
- Account for `SA_NODEFER`, guard splitting, and errno preservation as explicit invariants.
- Do not count a recovered runtime signal as the one crash for the process.
- Add a re-entry guard for crashes inside the app handler.
- Preserve errno across the app-first path and final chain path.
- Add tests for:
  - app handler gives up by restoring `SIG_DFL`
  - app handler recovers by `siglongjmp`
  - Mode B reports before recovery
  - registration through `sigaction`
  - registration through `signal`

### 5. Match dd-trace-c chaining and genuine-fault decisions

dd-trace-c does not report every delivered configured signal. It reports a genuine fault when siginfo is present and either:

- `si_code` is not `SI_USER` or `SI_TKILL`, or
- the async signal was sent by the process itself.

External `kill/tgkill` should not become crash telemetry by default.

After reporting, dd-trace-c chains carefully:

- `SIG_DFL` plus kernel-raised synchronous fault (`si_code > 0`): restore default and return, letting the faulting instruction re-execute so the kernel terminates with the original address and `si_code`.
- `SIG_DFL` plus async signal: restore default and `raise(sig)`.
- `SIG_IGN`: resume.
- function handler: invoke with the correct `SA_SIGINFO` calling convention.

Work needed:

- Add the genuine-fault filter.
- Fix default-disposition chaining to re-fault on synchronous faults rather than always calling `raise`.
- Preserve the original core-dump signal context.
- Keep current errno-preservation behavior.
- Add tests for external async signal, self-sent async signal, default sync re-fault, ignored disposition, and function handler chain.

### 6. Add whole-process lifetime and teardown semantics

dd-trace-c keeps crashtracking armed for the whole process by default. Bootstrap-only configuration tears it down at the end of bootstrap; otherwise the destructor calls `crashtracker_shutdown()` and force-restores handlers so unloaded code is not left installed as a signal handler.

Current libdatadog has `enable()`/`disable()`, but `disable()` does not restore old handlers and there is no equivalent preload uninstall/shutdown contract.

Without interposition, a disabled libdatadog handler can only chain to the handler displaced at install time. It cannot see a handler registered later by the application. That caveat belongs with the lifecycle work and reinforces the need for `sigaction`/`signal` virtualization.

Work needed:

- Add explicit init, bootstrap-end, and shutdown APIs for preload users.
- Default to whole-process lifetime.
- Support bootstrap-only lifetime.
- Restore effective app handlers on forced shutdown.
- Leave interposition transparent after teardown, or provide safe hook removal if available.
- Ensure no handler can dangle into unloaded code.

### 7. Add dd-trace-c env-driven preload configuration

dd-trace-c crashtracking can initialize from env without a language tracer constructing a `CrashtrackerConfiguration`.

Environment inputs used by the implementation:

- `DD_CRASHTRACKING_ENABLED=false` disables install.
- `DD_CRASHTRACKING_ALWAYS_ON_TOP=true` enables Mode B.
- `DD_CRASHTRACKING_ONLY_BOOTSTRAP=true` enables bootstrap-only lifetime.
- `DD_TRACE_LOG_LEVEL=debug` enables signal-safe debug breadcrumbs.
- `DD_TRACE_C_CRASHTRACKER_PROCESS` overrides the receiver path.
- `DD_SERVICE`, `DD_ENV`, `DD_VERSION`, `DD_INJECT_SENDER_TYPE` feed metadata.
- runtime id is snapshotted from dd-trace-c process metadata during init.
- `DD_TRACE_AGENT_URL` is intentionally resolved by the receiver through libdatadog config, not parsed in the in-process collector.

The fixed collector config emitted by dd-trace-c is:

- `additional_files: []`
- `create_alt_stack: false`
- `use_alt_stack: false`
- `demangle_names: true`
- `endpoint: null`
- `resolve_frames: EnabledWithSymbolsInReceiver`
- signals: `SIGSEGV`, `SIGABRT`, `SIGBUS`, `SIGILL`, `SIGFPE`
- timeout: 5 seconds
- `unix_socket_path: null`

Work needed:

- Add a preload-oriented config builder or C ABI that caches this state before arming the handler.
- Snapshot env-derived strings during init; do not call `getenv` from the signal path.
- Keep the config JSON stable and test it as a wire contract.
- Decide how to preserve libdatadog's general rule against hidden env reads for normal library callers while still supporting preload bootstrap.

### 8. Add receiver path discovery and loader-env scrubbing

dd-trace-c receiver path lookup order:

1. `DD_TRACE_C_CRASHTRACKER_PROCESS`
2. sibling of the loaded `libdd_autoinstrument.so`, preferring an architecture-suffixed receiver name such as `process-crash-receiver-linux-amd64`
3. sibling plain `process-crash-receiver`
4. baked default install path

The Rust port keeps a small C glue file because stable Rust cannot express weak `dladdr`/`dl_iterate_phdr` linkage the same way. It also canonicalizes when possible and checks executability.

Before `execv`, dd-trace-c strips `LD_PRELOAD` and `LD_AUDIT` from `environ` so the receiver does not re-run the preload constructor and recurse. If libdatadog uses an explicit `execve` environment list instead of inheriting `environ`, the equivalent fix is to construct the receiver environment without loader-injection variables while still preserving Datadog config needed by the receiver, such as agent URL/API-key settings.

Work needed:

- Add receiver path discovery usable by preload integrations.
- Support non-UTF-8 paths or explicitly document a UTF-8-only limitation.
- Add weak `dladdr`/`dl_iterate_phdr` C glue or another glibc-old-safe resolver.
- Ensure the receiver process does not inherit `LD_PRELOAD` or `LD_AUDIT`: strip them in the child if inheriting `environ`, or exclude them while constructing the explicit receiver env list.
- Add tests for explicit override, sibling suffixed receiver, sibling plain receiver, baked default, and preload recursion prevention.

### 9. Align metadata tags with dd-trace-c/dd-trace-py

dd-trace-c emits metadata with:

- `library_name: dd-trace-c`
- `library_version: <DD_TRACE_C_VERSION>`
- `family: native`
- tags:
  - `language:native`
  - `runtime:native`
  - `is_crash:true`
  - `severity:crash`
  - `service:<DD_SERVICE or default service>`
  - `env:<DD_ENV>` when set
  - `version:<DD_VERSION>` when set
  - `runtime_id:<snapshot>`
  - `runtime_version:<DD_TRACE_C_VERSION>`
  - `library_version:<DD_TRACE_C_VERSION>`
  - `platform:<DD_INJECT_SENDER_TYPE or host>`
  - `injector_version:<DD_TRACE_C_VERSION>`

Current libdatadog accepts caller-provided metadata; it does not provide this dd-trace-c preload metadata builder.

Work needed:

- Add a helper to build native preload metadata with this exact tag set.
- Snapshot runtime id before crash handling.
- Derive the default service name without doing signal-path work.
- Test telemetry/log tags and crash-report metadata against dd-trace-c expectations.

### 10. Make Linux unwinding optional and add a signal-safe fallback

Current libdatadog has Linux local libunwind in the forked collector and remote libunwind in the receiver. dd-trace-c's current `crashtracker_native` does not use libunwind in the collector; it walks frame pointers from `ucontext` and probes frame records with `process_vm_readv` on itself. The old C implementation used a `sigsetjmp`/`siglongjmp` recovery handler around raw frame-pointer reads; the Rust port replaced that with `process_vm_readv` so corrupt frame pointers return `EFAULT` instead of crashing the collector.

This is not just dependency cleanup. Current libdatadog's collector child walks the crashing thread stack with local libunwind. A corrupt frame can fault in the collector child; combined with the current lack of child crash-signal reset, that can re-enter the inherited crash handler. The frame-pointer plus no-fault memory-read path is a crash-safety requirement for preload mode.

Work needed:

- Feature-gate or config-gate local libunwind in the collector.
- Provide a frame-pointer fallback for x86_64 and aarch64.
- Use `process_vm_readv` probing, or another explicit no-fault memory-read strategy, for frame records.
- Treat errno-returning `process_vm_readv` failures, such as `EPERM`, as graceful stacktrace loss. Document that `SECCOMP_RET_KILL` is different: it kills the collector clone and may lose the report entirely.
- Consider a later preflight probe for `process_vm_readv`, for example a sacrificial child at init that calls `process_vm_readv` on itself and records whether the syscall returns normally, returns `EPERM`, or is killed by seccomp. This is useful follow-up work, not required for the first signal-safe collector cut.
- Keep remote libunwind for receiver all-thread collection separate from local collector unwinding.
- Decide whether `EnabledWithInprocessSymbols` is allowed in the signal/fork-child path for preload use. It likely should not be the default there.
- Add tests for null ucontext, corrupt frame pointer, seccomp-denied memory probe if practical, and fallback without libunwind.

### 11. Align signal set and si_code wire behavior

dd-trace-c manages `SIGSEGV`, `SIGABRT`, `SIGBUS`, `SIGILL`, and `SIGFPE`. Current libdatadog defaults are `SIGBUS`, `SIGABRT`, `SIGSEGV`, and `SIGILL`; `SIGFPE` is not included by default.

dd-trace-c deliberately maps `SIGFPE` `FPE_*` si_codes to `UNKNOWN` because libdatadog's `SiCodes` enum has no `FPE_*` variants and the receiver deserializes `si_code_human_readable`.

Work needed:

- Decide whether libdatadog default signals should include `SIGFPE`.
- If `SIGFPE` is enabled, keep `FPE_*` as `UNKNOWN` until `SiCodes` and receiver deserialization support those variants.
- Add tests for `SIGFPE` reporting and receiver parsing.
- Keep emitted signal and si_code strings compatible with receiver enum names.

### 12. Add signal-safe debug logging

dd-trace-c has a debug path that formats into a fixed stack buffer and calls `dd_trace_log_write_signal` exactly once. It is gated by a cached `DD_TRACE_LOG_LEVEL` read from init.

Work needed:

- Add a fixed-buffer signal-safe debug writer for the crash path.
- Do not call normal logger paths from signal context.
- Keep normal init/teardown logs separate from crash-path logs.
- Add debug breadcrumbs for install, app-first, genuine-fault decision, collector spawn, duplicate collection, and chain action.

## P1 work

### 13. Make the wire emitter deterministic and allocation-free for preload

dd-trace-c's preload collector emits a minimal stream:

1. config
2. metadata
3. additional tags
4. kind
5. siginfo
6. procinfo
7. stacktrace
8. message
9. done

Current libdatadog's full emitter emits more sections and uses std formatting/serialization. The receiver can parse flexible order, but preload should have a small deterministic emitter that cannot allocate.

Work needed:

- Add a preload/minimal emitter, or make current emitters swappable by crash mode.
- Use fixed scratch buffers for each JSON section.
- Avoid protocol injection in message and tags.
- Keep section names compatible. The receiver already accepts dd-trace-c's `PROCESSINFO` spelling; add a golden round-trip test instead of treating this as parser work.
- Decide how to preserve crash-ping enrichment if metadata is emitted before message.

### 14. Preserve and test fd/socket protocol choices

dd-trace-c uses `pipe()` and execs the receiver with the read end on stdin. libdatadog's general collector uses `socketpair()` and wraps it in `UnixStream`.

Work needed:

- Decide whether preload mode should use dd-trace-c's pipe+stdin model for simplicity and safety.
- If socketpair remains, implement a raw-fd sink and raw poll/reap path without `UnixStream`.
- Ensure EOF signaling and receiver completion are deterministic.
- Add tests for closed stdio descriptors, low-numbered pipe/socket fds, `EINTR` write retries, short writes, and receiver EOF.

### 15. Packaging and build integration

dd-trace-c builds and ships:

- `libdd_autoinstrument.so` with the in-process crashtracker staticlib linked in.
- `process-crash-receiver` as a musl/self-contained receiver sidecar.
- Size guard for receiver sidecar, currently 5 MiB in dd-trace-c.
- Build-time `DD_TRACE_C_VERSION` and install path injection.
- `crashtracker_glue.c` for weak self `.so` path resolution and log gate helpers.

Work needed:

- Decide whether the signal-safe collector lives inside `libdd-crashtracker`, a new crate, or an integration crate consumed by dd-trace-c.
- Add builder/release artifact support if it becomes a libdatadog-owned artifact.
- Add musl receiver build knobs and self-contained libunwind/libc link handling.
- Add receiver size checks if libdatadog owns the sidecar packaging.
- Keep old-glibc load behavior intact; avoid hard `libdl` symbols.

### 16. API boundaries and ownership

There are two plausible designs:

- Put the generic signal-safe collector in libdatadog and let integrations provide metadata, receiver-path, and hook-engine adapters.
- Put only reusable primitives in libdatadog and keep dd-trace-c's preload-specific policy in dd-trace-c.

Either way, libdatadog needs a clearer split between:

- general crashtracker API for language runtimes
- preload/constructor API
- receiver-only API
- signal-safe low-level primitives
- std/async receiver and upload code

Work needed:

- Define which API reads env vars and which never does.
- Define which API is allowed to install global signal handlers.
- Define which API is allowed to interpose `sigaction`/`signal`.
- Define teardown ownership for signal handlers and hook tables.
- Document all async-signal-safety contracts on public callbacks.

## P2 work

### 17. Backfill integration tests from dd-trace-c

Port or mirror these dd-trace-c `test/agentapi/crashtracker_preload_test.go` scenarios:

- genuine crash during injector init uploads a crash report
- crash in dd-trace-c's own HTTP client is labeled `http_client_send`
- `DD_CRASHTRACKING_ENABLED=false` skips install
- stuck receiver is killed and reaped
- app handler gives up, crashtracker reports
- app handler recovers, crashtracker does not report in Mode A
- `DD_CRASHTRACKING_ALWAYS_ON_TOP=true` reports before recovery
- app uses `signal()` instead of `sigaction()`
- whole-process default reports application crash
- `DD_CRASHTRACKING_ONLY_BOOTSTRAP=true` does not report application crash

Add new libdatadog-specific tests for:

- late handler registration after crashtracker install
- raw `rt_sigaction` limitation documented and tested if practical
- receiver exec environment excludes `LD_PRELOAD` and `LD_AUDIT`
- receiver path beside `.so`, including suffixed artifacts
- closed stdio fd preservation
- `SIGFPE` and `UNKNOWN` si_code parsing
- external async signal ignored
- self-sent async signal reported
- default sync re-fault preserves crash signal context
- golden preload wire round-trip through the receiver, including `PROCESSINFO`
- forked collector has no allocator/logging/std calls on the hot path, including no `eprintln!`, no `std::fs::File`, and no `dlsym`

### 18. Keep existing libdatadog strengths

While adding dd-trace-c parity, avoid regressing current libdatadog functionality:

- all-thread collection
- unhandled exception reporting
- runtime callback stacks
- counters, spans, traces, and additional files
- Windows collector behavior
- macOS collector behavior
- receiver telemetry debug logs
- errors-intake output

Preload mode can be smaller and stricter than general mode, but the two should share receiver data models where practical.

## Suggested PR split

1. Document the signal-safety model and split the collector path into `std` and signal-safe modules.
2. Add raw syscall/fd/sink primitives with tests.
3. Add allocation-free minimal emitter and golden wire tests.
4. Add frame-pointer/process_vm_readv fallback and libunwind feature/config split.
5. Add receiver child env/fd sanitation and bounded reap semantics.
6. Add preload metadata builders.
7. Add sigaction/signal virtualization and Mode A/Mode B. Do not implement Mode A before the virtualized effective-handler state exists.
8. Add preload env init and receiver path discovery.
9. Port dd-trace-c integration tests.
10. Decide packaging ownership and wire into builder/release artifacts.

## Quick risk ranking

Highest risk missing pieces:

1. Forked collector child not being treated as async-signal-safe.
2. Linux local unwinding in the collector can re-fault on a corrupt crashing stack and lacks the dd-trace-c frame-pointer/process_vm_readv fallback.
3. No late `sigaction`/`signal` virtualization.
4. No Mode A app-first policy for managed-runtime recovery.
5. Default-disposition chaining via `raise` instead of re-fault for synchronous kernel faults.
6. Receiver exec environment can recurse under `LD_PRELOAD`/`LD_AUDIT` unless the receiver env is sanitized or explicitly built without loader vars.
7. Preload metadata/report shape is not available as a libdatadog helper.
