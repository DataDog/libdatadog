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

- **Collect all thread stacks near crash time** with function names,
  IPs, and SPs for every active thread in the process
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

When `collect_all_threads` is `false`, only the crashing thread's stack
trace is collected and the receiver does not attempt ptrace.

## Design

### Two-Phase, Two-Process Architecture (Linux)

Multi-thread collection is split across three actors (crashing process, fork, receiver) to maintain
signal-handler safety:

### Phase 1: Signal Handler (Async-Signal-Safe)

The signal handler MUST only perform async-signal-safe operations:

- `prctl(PR_SET_PTRACER, receiver_pid)` — grants ptrace permission to
  the receiver, scoped to that single PID
- `fork()` — spawns the collector child
- `read()`/`write()` on pre-allocated pipe/socket file descriptors
- `getsockopt(SO_PEERCRED)` — verifies receiver identity in sidecar mode
- Atomic pointer swaps for state coordination

The handler MUST NOT enumerate threads, call `dladdr`, allocate memory,
or perform any unwinding. It MUST remain blocked (keeping the process
alive as a ptrace target) until the receiver signals completion.

### Phase 2: Collector Child (Forked Process)

After fork, the collector child:

1. Unwinds the **crashing thread only** using the kernel-saved
   `ucontext`:
   - **Linux:** `unw_init_local2(cursor, ucontext, UNW_INIT_SIGNAL_FRAME)`
     seeded from the saved CPU state at the moment of the crash. The
     signal-frame flag (`1`) is required so that libunwind knows the
     cursor starts inside a signal trampoline and applies the correct
     return-address adjustment (without it, the first frame's IP may be
     off-by-one or the unwind may miss the faulting frame entirely).
     Then `unw_step()`/`unw_get_reg()` loop up to 512 frames.
   - **macOS:** Frame-pointer walk from `__ss.__pc`/`__rip` and
     `__ss.__rbp`/`__fp`, validated against pthread stack bounds
2. Emits `ProcInfo` containing parent PID and crashing TID
   (`SYS_gettid` on Linux)
3. Emits `/proc/self/maps` contents for later symbolization
4. Streams all data to the receiver over the pipe/socket

### Phase 3: Receiver — Thread Collection

After consuming the collector's stream, the receiver proceeds with
background thread collection if `collect_all_threads()` is enabled:

#### 3a. Thread Enumeration

The receiver MUST enumerate threads by reading
`/proc/{parent_pid}/task/`. Each entry is a numeric TID. The crashing
TID (from `ProcInfo`) is filtered out since its stack is already in
`error.stack`.

#### 3b. Thread Suspension via Ptrace

For each thread (up to `max_threads`), the receiver:

1. **Attaches** with `PTRACE_SEIZE(tid, PTRACE_O_TRACESYSGOOD)` —
   unlike `PTRACE_ATTACH`, this does not deliver SIGSTOP
2. **Interrupts** with `PTRACE_INTERRUPT(tid)` — causes the thread to
   enter a ptrace-stop without a signal
3. **Waits** with `waitpid(tid, WNOHANG | __WALL)` in a polling loop
   with a per-thread timeout (50 ms)

The use of `PTRACE_SEIZE` + `PTRACE_INTERRUPT` rather than
`PTRACE_ATTACH` + `SIGSTOP` is deliberate: it avoids delivering
user-visible signals to threads and does not interact with the target's
signal handlers.

#### 3c. Remote Stack Unwinding

While a thread is ptrace-stopped, the receiver unwinds its stack:

1. A single `unw_create_addr_space(&_UPT_accessors)` is created once
   and shared across all threads (DWARF `.eh_frame` cache reuse)
2. Per thread: `_UPT_create(tid)` → `unw_init_remote(cursor, space,
   upt_info)` → `unw_step()` loop collecting IP and SP per frame
3. If `StacktraceCollection::EnabledWithSymbolsInReceiver` is
   configured, `unw_get_proc_name_remote()` resolves function names
   during the unwind
4. `_UPT_destroy(upt_info)` releases per-thread state

#### 3d. Thread Metadata

For each collected thread, the receiver reads
`/proc/{pid}/task/{tid}/stat` to extract:
- Thread name (field 2, in parentheses)
- Thread state (field 3: R/S/D/Z/T)

#### 3e. Detach

After unwinding, `PTRACE_DETACH(tid)` releases the thread. The receiver
MUST detach all threads before signaling completion to the parent,
regardless of errors during unwinding.

### Windows Implementation

Windows does not use the two-process model. In the WER
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

Non-crashing threads appear in `error.threads[]` (see RFC 0011 v1.7+):

```json
{
  "error": {
    "stack": { "...crashing thread..." },
    "threads": [
      {
        "crashed": false,
        "name": "worker-pool-3",
        "state": "S",
        "stack": {
          "format": "libunwind-ptrace",
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

On Linux, the signal handler calls `prctl(PR_SET_PTRACER, receiver_pid)`
to grant ptrace permission **only** to the receiver process. This is the
minimum privilege needed.

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

- The per-thread timeout (50 ms waitpid polling) expires for a given
  thread — that thread is skipped
- The overall time budget is exhausted
- `max_threads` is reached

When collection is cut short, the receiver:
- Emits all threads that were successfully collected
- Sets `counters.threads_incomplete = 1` in the crash report metadata
- Sets `stack.incomplete = true` on any thread whose unwind was
  interrupted

This ensures partial data is always preferable to no data, and consumers
can detect incomplete collection.
