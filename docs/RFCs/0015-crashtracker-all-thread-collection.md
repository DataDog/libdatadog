# RFC 0015: Crashtracker All-Thread Collection

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT",
"SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this
document are to be interpreted as described in
[IETF RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

**Date:** June 15, 2026

## Summary

This RFC documents the architecture for collecting stack traces from all
threads in a crashing process—not just the crashing thread. On Linux,
the receiver process enumerates threads via `/proc/{pid}/task/`, attaches
with `PTRACE_SEIZE`, and unwinds each thread remotely using
libunwind-ptrace.

## Problem

When a multi-threaded application crashes, knowing only the crashing
thread's stack trace is often insufficient to diagnose the root cause.
Concurrency bugs—data races, deadlocks, lock-order inversions—manifest
as a crash on one thread caused by state corruption on another.

## Goals

- **Collect all thread stacks near crash time** with IPs and SPs for
  every active thread in the process, and with function names when
  `StacktraceCollection::EnabledWithSymbolsInReceiver` is configured
- **Preserve signal-handler safety:** No thread enumeration, ptrace, or
  heap allocation in the signal handler
- **Bounded resource usage:** Configurable caps on thread count and time
  budget to prevent unbounded collection in large-threadpool processes
- **Opt-in by default:** Multi-thread collection is disabled unless
  explicitly enabled via configuration
- **Security:** Ptrace permissions are scoped to the verified receiver
  process only

## Non-Goals

- macOS multi-thread collection (no ptrace-based remote unwind path
  exists today)
- Core-dump generation or full register state per thread
- Thread synchronization replay or happens-before analysis
- Collecting thread-local storage or heap contents

## Configuration

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `collect_all_threads` | `bool` | `false` | Enable multi-thread collection. |
| `max_threads` | `usize` | `256` | Maximum number of background threads to collect. |
| `timeout` | `Duration` | receiver timeout | Time budget for the entire receiver phase (shared with other post-processing). |
| `resolve_frames` | `StacktraceCollection` | `Disabled` | Where — and whether — symbols are resolved. |

When `collect_all_threads` is `false`, only the crashing thread's stack
trace is collected and the receiver does not attempt ptrace.

`collect_all_threads` and `resolve_frames` are independent. Thread
collection is gated solely on the former, so enabling it with
`resolve_frames` set to anything other than
`EnabledWithSymbolsInReceiver` yields thread stacks that are never
symbolized. Conversely, for a **signal crash**, `resolve_frames =
Disabled` combined with `collect_all_threads` populates `error.threads`
while leaving `error.stack` empty, because the crashing thread's unwind
is gated on `resolve_frames` while the thread walk is gated only on
`collect_all_threads`. This does not apply to unhandled exceptions,
whose runtime-supplied stack is emitted regardless of `resolve_frames`.

## Design

### Three-Process Architecture (Linux)

Multi-thread collection is split across three processes (crashing
process, collector child, receiver) to maintain signal-handler safety:

### Phase 1: Signal Handler (Async-Signal-Safe)

The signal handler MUST only perform async-signal-safe operations:

- `prctl(PR_SET_PTRACER, receiver_pid)` — grants ptrace permission to
  the receiver, scoped to that single PID
- `fork()` — spawns the collector child
- `read()`/`write()` on pre-allocated pipe/socket file descriptors
- `getsockopt(SO_PEERCRED)` — verifies receiver identity in sidecar mode
- Atomic pointer swaps for state coordination

The handler MUST NOT enumerate threads, call `dladdr`, allocate memory,
or perform any unwinding. It MUST remain blocked, keeping the process
alive as a ptrace target, until the receiver closes the connection. That
close happens once thread collection has finished, so the process stays
available for the ptrace walk but not for the symbolization that follows
it (see 3c).

### Phase 2: Collector Child (Forked Process)

After fork, the collector child:

1. Unwinds the **crashing thread only** using the kernel-saved
   `ucontext`, unless `resolve_frames` is `Disabled`, in which case no
   stack is emitted at all:
   - **Linux:** `unw_init_local2(cursor, ucontext, UNW_INIT_SIGNAL_FRAME)`
     seeded from the saved CPU state at the moment of the crash. The
     signal-frame flag (`1`) is required so that libunwind knows the
     cursor starts inside a signal trampoline and applies the correct
     return-address adjustment (without it, the first frame's IP may be
     off-by-one or the unwind may miss the faulting frame entirely).
     Then `unw_step()`/`unw_get_reg()` loop up to 512 frames.
   - **macOS:** Frame-pointer walk from `__ss.__pc`/`__rip` and
     `__ss.__rbp`/`__fp`, validated against pthread stack bounds

   Every frame carries `ip`, and on Linux also `sp` and `fp`. `dladdr`
   is called unconditionally on both platforms for
   `module_base_address` and `symbol_address`. On Linux a `function` name
   is added only under `EnabledWithInprocessSymbols`, via
   `unw_get_proc_name()`; on macOS `dladdr`'s `dli_sname` is used as the
   name for every mode that emits a stack.

2. Emits `ProcInfo` containing parent PID and crashing TID
   (`SYS_gettid` on Linux)
3. Emits `/proc/self/maps` contents, which are attached to the report as
   a file. Symbolization does not read this attachment; the receiver
   reads the live `/proc/{pid}/maps` instead.
4. Streams all data to the receiver over the pipe/socket

An unhandled exception reported through `report_unhandled_exception`
follows a **two-process** path rather than the three-process one above:
it does not fork a collector child, and instead writes the report
directly from the crashing process into the receiver connection. There is
no `ucontext` and no native unwind of the reporting thread.

The runtime-supplied stack is emitted to `error.stack` unchanged and
regardless of `resolve_frames`. Under `EnabledWithSymbolsInReceiver` the
receiver then enriches it like any other stack, adding normalized fields
and replacing a supplied `function` where symbolization succeeds, so the
uploaded stack can differ from what the runtime provided. The native view
of every thread — the reporting thread included — comes from the
receiver's ptrace walk.

### Phase 3: Receiver — Thread Collection

After consuming the collector's stream, the receiver proceeds with
background thread collection if `collect_all_threads()` is enabled:

#### 3a. Thread Enumeration

The receiver MUST enumerate threads by reading
`/proc/{parent_pid}/task/`. Each entry is a numeric TID.

The crashing TID (from `ProcInfo`) is **included**, and MUST be
processed first so that the `max_threads` cap can never drop it. It
appears in `error.threads` marked `crashed: true`, giving consumers a
uniform native view of every thread. For a signal crash with
`resolve_frames != Disabled` this means the crashing thread is
represented twice: `error.stack` holds the unwind seeded from the
kernel-saved `ucontext`, while its `error.threads` entry holds the ptrace
walk taken with the thread parked inside the signal handler. Under
`Disabled` there is no `ucontext` unwind, so the crashing thread appears
only in `error.threads`.

#### 3b. Thread Suspension via Ptrace

For each thread (up to `max_threads`), the receiver:

1. **Attaches** with `PTRACE_SEIZE(tid)` (no options)
2. **Interrupts** with `PTRACE_INTERRUPT(tid)` — causes the thread to
   enter a ptrace-stop without a signal
3. **Waits** with `waitpid(tid, WNOHANG | __WALL)` in a 2 ms polling
   loop, bounded by a per-thread stop timeout of 200 ms (itself capped
   by the overall budget)
4. **Waits for registers to be committed**, best-effort, by polling
   `PTRACE_PEEKUSER` for a non-zero instruction pointer. On older kernels
   `waitpid` can report the stop before register state is readable, which
   would otherwise yield an empty stack. `EIO` means the architecture does
   not support the probe and the check is skipped; libunwind uses
   `PTRACE_GETREGSET`, which works regardless.

   This is a wait, not a guarantee. The result is discarded and the unwind
   proceeds even when the deadline expires without a non-zero IP, because
   detaching and re-attaching costs more than an empty capture. The
   zero-frame retry below is what actually covers that case.

The use of `PTRACE_SEIZE` + `PTRACE_INTERRUPT` rather than
`PTRACE_ATTACH` + `SIGSTOP` is deliberate: it avoids delivering
user-visible signals to threads and does not interact with the target's
signal handlers.

Attachment is retried up to three times with exponential backoff
(10 ms, 20 ms, 40 ms) on timeout — four attempts in total — and each
attempt gets a fresh per-thread budget, so a single thread can consume up
to four 200 ms stop timeouts plus 70 ms of backoff before being skipped.
A capture that succeeds but yields zero frames is
also retried, since a running thread with a confirmed non-zero IP should
always produce at least one frame. `EPERM` (Yama denial or a missing
`PR_SET_PTRACER`) and `ESRCH` (thread already exited) are permanent and
are not retried.

#### 3c. Remote Stack Unwinding

While a thread is ptrace-stopped, the receiver unwinds its stack:

1. A single `unw_create_addr_space(&_UPT_accessors)` is created once
   and shared across all threads (DWARF `.eh_frame` cache reuse)
2. Per thread: `_UPT_create(tid)` → `unw_init_remote(cursor, space,
   upt_info)` → `unw_step_remote()` loop collecting IP and SP per frame,
   up to 512 frames
3. `_UPT_destroy(upt_info)` releases per-thread state

Only instruction and stack pointers are collected during the unwind.
Under `StacktraceCollection::EnabledWithSymbolsInReceiver` the receiver
is the single place that resolves symbols, and it does so for every
thread — the crashing one included — via blazesym in
`CrashInfo::enrich_callstacks`. libunwind's remote symbol lookup
(`unw_get_proc_name_remote()`) MUST NOT be used: it can segfault the
receiver, which costs the entire crash report rather than a single
function name.

`enrich_callstacks` has two phases and shares one `Symbolizer` between
them:

1. **Normalize.** Each frame's absolute IP is translated into a file
   `path` plus `relative_address` by reading `/proc/{pid}/maps`. Opening
   a mapped file also registers an `ElfResolver` with the symbolizer,
   keyed by that path.
2. **Resolve.** Each frame is symbolized through `Source::Process` with
   `Input::AbsAddr`. When that yields no symbol and the frame carries a
   `path` and `relative_address`, it is retried against `Source::Elf`
   with `Input::VirtOffset`, reusing the resolver registered in phase 1.

The process source needs `/proc/{pid}` to exist. The ELF retry reads the
binary from disk and needs nothing from the live process, which is what
keeps function names available once the process is gone.

Whether that matters depends on how the receiver was reached:

- **Unix-socket sidecar** (`async_receiver_entry_point_unix_listener` and
  `async_receiver_entry_point_unix_socket`): the crashing process is
  released *before* symbolization. `receive_report_from_stream` owns the
  `UnixStream` and drops it on return, delivering the POLLHUP the crashing
  process is waiting on in `wait_for_pollhup`. Thread collection happens
  inside that function, so unwinding sees a live process, but both
  symbolization phases race the process's teardown.
- **Fork/exec receiver** (`receiver_entry_point_stdin`): the stream wraps
  `tokio::io::stdin()`, and dropping that handle does not close fd 0. The
  peer therefore stays open until the receiver process exits, after
  symbolization and upload, so the crashing process is still available
  throughout.

Normalization reads `/proc/{pid}/maps` and so depends on that process.
A frame that fails to normalize has no ELF location to retry against and
stays unsymbolized.

Symbolization failure MUST NOT cost the report. A frame that resolves
through neither source keeps its `ip`, `sp`, and whatever normalization
recorded; the error is appended to the frame's `comments`; and the
receiver still uploads.

#### 3d. Thread Metadata

For each collected thread, the receiver reads
`/proc/{pid}/task/{tid}/stat` to extract:
- Thread name (field 2, in parentheses)
- Thread state (field 3: R/S/D/Z/T)

#### 3e. Detach

After unwinding, `PTRACE_DETACH(tid)` releases the thread. Each thread is
detached as soon as its own unwind finishes, regardless of errors during
that unwind, so every thread is released before the receiver closes the
connection to the parent.

Detach is best-effort: a failure is not allowed to discard an otherwise
good stack trace, and `ESRCH` is treated as success because the thread
has already exited. Any pending `waitpid` event is drained afterwards so
the kernel fully releases the thread; without that, a rapid re-attach
can fail with `EPERM` under CPU pressure.

### Windows Implementation

Windows does not use the multi-process model. In the WER
`exception_event_callback`:

1. **Enumerate:** `CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, pid)` +
   `Thread32First`/`Thread32Next`, filtering by owning PID
2. **Context:** `OpenThread(THREAD_ALL_ACCESS)` +
   `GetThreadContext(CONTEXT_FULL)`
3. **Walk:** `StackWalkEx` from `DbgHelp.dll`, resolving modules from a
   pre-enumerated module list

All threads are always collected (no opt-in flag). The crashing thread
is identified by matching against the exception thread handle.

### macOS

Only the crashing thread is collected via frame-pointer walk in the
forked collector child. Multi-thread collection is not implemented.

## Data Structures

### Output Format

On Linux every collected thread appears in `error.threads[]` (see RFC
0011 v1.7+), including the crashing one, which is marked `crashed:
true`. On Windows the crashing thread is placed in `error.stack` and
only the others become `ThreadData` entries.

`format` is always the `StackTrace` format string, `"Datadog
Crashtracker 1.0"`, regardless of how the frames were captured.

For Linux ptrace-collected thread frames, the `function`, `path`,
`relative_address`, and `build_id` fields below are present only under
`EnabledWithSymbolsInReceiver`; in the other modes those frames carry
`ip` and `sp` alone. This gating is Linux-specific: `walk_thread_stack`
on Windows is not conditioned on `resolve_frames` and fills in
`module_base_address`, `symbol_address`, and `path` from the
pre-enumerated module list for every captured thread.

```json
{
  "error": {
    "stack": { "...crashing thread, unwound from ucontext..." },
    "threads": [
      {
        "crashed": true,
        "name": "main",
        "state": "S",
        "stack": {
          "format": "Datadog Crashtracker 1.0",
          "frames": [
            { "ip": "0x55a3f2c05678", "sp": "0x7ffd1c002a40", "function": "faulting_fn" }
          ],
          "incomplete": false
        }
      },
      {
        "crashed": false,
        "name": "worker-pool-3",
        "state": "S",
        "stack": {
          "format": "Datadog Crashtracker 1.0",
          "frames": [
            { "ip": "0x7f2a1b3c4d50", "sp": "0x7f2a0c001e80", "function": "pthread_cond_wait" },
            { "ip": "0x55a3f2c01234", "sp": "0x7f2a0c001ec0", "function": "worker_loop" }
          ],
          "incomplete": false
        }
      }
    ]
  }
}
```

### Internal Structures

```rust
pub struct ThreadData {
    pub crashed: bool,
    pub name: String,
    pub stack: StackTrace,
    pub state: Option<String>,
}

pub struct CapturedThreadContext {
    pub stack_trace: StackTrace,
}
```

## Security

### Ptrace Permission Scoping

On Linux, both crash entry points — the signal handler and
`report_unhandled_exception` — call `prctl(PR_SET_PTRACER,
receiver_pid)` to grant ptrace permission **only** to the receiver
process. This is the minimum privilege needed. The grant is made only
when `collect_all_threads` is enabled.

### Sidecar Mode Verification

When the receiver is a long-running sidecar process (not freshly
spawned), the signal handler MUST verify the receiver's identity:

1. The expected receiver PID is registered in advance via
   `set_expected_receiver_pid()`
2. At crash time, the handler reads `SO_PEERCRED` from the Unix socket
3. If the peer PID does not match the expected PID, ptrace permission is
   **not granted** (fail-closed)

This prevents a compromised or replaced sidecar from gaining ptrace
access to the crashing process.

## Timeout and Partial Collection

Collection is **best-effort**. The receiver uses the remaining time
budget after parsing the crash stream from stdin. Collection stops early
if:

- The per-thread stop timeout (200 ms of waitpid polling) expires for a
  given thread on all four attempts — the initial one plus three retries
  — and that thread is skipped
- The overall time budget is exhausted
- `max_threads` is reached. The crashing thread is collected first and
  so is never the one dropped

When collection is cut short, the receiver:
- Emits an entry for every thread it visited, including ones whose
  capture failed; those carry an empty frame list with
  `stack.incomplete = true`. Threads never visited are absent entirely
- Sets `counters.threads_incomplete = 1` in the crash report metadata

A remote unwind that succeeds is always reported with
`stack.incomplete = false`, including when it stops at the 512-frame
cap.

This ensures partial data is always preferable to no data, and consumers
can detect incomplete collection.
