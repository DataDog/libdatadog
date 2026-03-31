# RFC 0014: Crashtracker Crash Pre-Diagnosis

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT",
"SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this
document are to be interpreted as described in
[IETF RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

**Date:** March 26, 2026

## Summary

This RFC proposes adding automated crash diagnosis capabilities to the crashtracker. Instead of only collecting raw crash data (signals, registers, memory maps, stack traces), the system will correlate these data sources to provide structured diagnostic conclusions like "null pointer dereference" or "stack overflow." This diagnosis will be computed in the backend errors processing pipeline and included in crash reports.

## Problem

When a crash report arrives today, it contains all the raw data needed to understand what happened: the signal and its code, the fault address (si_addr), the full memory map (/proc/self/maps), the CPU register state (ucontext), and the stack trace. However, these pieces are emitted independently and nobody correlates them. A human must manually cross-reference the fault address against the memory map, check register validity, and reason about the signal code to reach a conclusion like "this was a null pointer dereference" or "this was a stack overflow."

This analysis is straightforward to automate. The crashtracker has all of this data before it uploads the crash report. We can compute a structured diagnosis and include it in the payload, giving users a preliminary actionable direction instead of just raw data.

## Goals

- **Correlate existing data:** Cross-reference sig_info, ucontext registers, and /proc/self/maps to produce a structured diagnosis
- **Classify common crash patterns:** Null dereference, stack overflow, write-to-read-only, use-after-free, illegal instruction, etc.
- **Enrich the payload:** Add a diagnosis object to the crash report so that UIs can surface root cause directly
- **Zero collector signal handler risk:** All analysis runs in the receiver process or backend, not in the signal handler

## Non-Goals

- Provide a proof engine (this is a heuristic-based classifier)
- Full symbolic debugging or core-dump-level analysis
- Windows/macOS support initially (no /proc/self/maps equivalent; future work)

## Background: Current Crash Data

### Signal Info (sig_info)
Already structured. Contains:
- `si_signo / si_signo_human_readable` – 11 / SIGSEGV
- `si_code / si_code_human_readable` – 1 / SEGV_MAPERR
- `si_addr` – fault address as hex string – "0x0000000000000018"

### Memory Map (files["/proc/self/maps"])
Raw lines from /proc/self/maps, stored as `Vec<String>`. Each line:
```
55a3f2a00000-55a3f2c00000 r-xp 00000000 fd:01 1234567  /usr/bin/myapp
7ffc89a00000-7ffc89c00000 rw-p 00000000 00:00 0        [stack]
```

### CPU Registers (experimental.ucontext)
Stored as a Rust Debug-format string of ucontext_t. Contains all general purpose registers (RIP, RSP, RBP, RAX, etc. on x86_64) but in an unparsed format.

### Stack Trace (error.stack)
Structured frames with ip, sp, function, path, etc.

**THE GAP:** These four sources are never correlated with each other.

## Design

All diagnosis logic will run outside of the crashtracker signal handler. The backend errors processing pipeline is the natural insertion point, as crash reports already contain all data needed for diagnosis. This avoids any performance impact on the crashing application.

### Structured ucontext Requirements

As specified in [RFC 0011 schema version 1.6](./0011-crashtracker-structured-log-format-V1_X.md), the crashtracker library **MUST** now emit ucontext data in structured format for UNIX signal crashes. The raw debug string format is no longer sufficient for automated diagnosis.

**Required format (per RFC 0011 v1.6):**
```json
{
  "arch": "x86_64",
  "registers": {
    "rip": "0x55a3f2c01234",
    "rsp": "0x7ffc89abcdef",
    "rbp": "0x7ffc89abce00",
    "rax": "0x0000000000000000"
  },
  "raw": "ucontext_t { ... }"  // optional debug string
}
```

The collector **MUST** implement platform-specific handling:

**x86_64:** Extract registers from `mcontext_t.gregs[]` using constants like `REG_RIP`, `REG_RSP`, `REG_RBP`, etc.

**aarch64:** Extract registers from `mcontext_t` structure, mapping to canonical names like `"pc"`, `"sp"`, `"x0"`..`"x30"`.

**Diagnosis parser:** Map architecture-specific names to canonical fields:
```rust
let ip = registers.get("rip").or_else(|| registers.get("pc")).copied();
let sp = registers.get("rsp").or_else(|| registers.get("sp")).copied();
let fp = registers.get("rbp").or_else(|| registers.get("x29")).copied();
```

### Parse /proc/self/maps

In the backend, parse the raw lines into structured entries:

```rust
struct MemoryMap {
    entries: Vec<MemoryMapping>,
}

struct MemoryMapping {
    start: u64,
    end: u64,
    readable: bool,
    writable: bool,
    executable: bool,
    private: bool,
    offset: u64,
    pathname: Option<String>,  // "/usr/lib/libc.so.6", "[heap]", "[stack]"
}
```

### Diagnosis Engine

Given parsed inputs (sig_info, registers, memory_maps), produce a CrashDiagnosis using decision trees keyed on signal type and code, enriched by address lookups.

#### SIGSEGV Diagnosis

| si_code | Fault address analysis | Diagnosis |
|---------|----------------------|-----------|
| SEGV_MAPERR | addr < 1 page size | Null pointer dereference. Small offsets indicate field access on a null struct/object pointer. |
| SEGV_MAPERR | addr near [stack] guard page | Stack overflow. SP and/or fault address within one page of the stack mapping boundary |
| SEGV_MAPERR | addr unmapped, near [heap] | Probable use-after-free or heap corruption. Address is in the heap neighborhood but no longer mapped. |
| SEGV_MAPERR | addr unmapped, no pattern | Wild pointer / use-after-munmap. |
| SEGV_ACCERR | addr in r--p or r-xp region | Write to read-only memory. |
| SEGV_ACCERR | addr in rw-p region, but IP tried to execute it | Executing non-executable memory. |

#### SIGBUS Diagnosis

| si_code | Analysis | Diagnosis |
|---------|----------|-----------|
| BUS_ADRALN | — | Misaligned memory access. |
| BUS_ADRERR | addr in file-backed mapping | Access beyond end of memory-mapped file. File was truncated or mapping extends past EOF. |

#### SIGABRT Diagnosis
Intentional abort. Typically from assert(), panic!, or allocator-detected corruption (double free, heap buffer overflow detected by guard). The error message field should contain the specific reason.

#### SIGILL Diagnosis

| IP analysis | Diagnosis |
|-------------|-----------|
| IP in r-xp mapped region | Illegal instruction. CPU encountered an invalid opcode; possible ABI mismatch, compiler bug, or corrupted code section. |
| IP not in any executable region | Jumped to non-executable memory. Corrupted return address, vtable, or function pointer. |

#### Additional Register-based Checks
When structured ucontext is available:
- **Stack pointer validity:** Is RSP/SP within a [stack] mapping? If not, the stack pointer itself is corrupted, which changes the diagnosis significantly.
- **Near-null register scan:** Which registers contain values in the 0–1 page range? These are likely the null pointer that was dereferenced. Reporting "RAX=0x0, fault at offset 0x18" tells the developer which variable was null.
- **Frame pointer chain validity:** Is RBP within a stack mapping? Broken frame pointer suggests stack corruption.

### Output Schema

The diagnosis is added as a new optional field on CrashInfo:

```rust
diagnosis: {
    summary: String,
    category: String,       // enum "NullPointerDereference", "StackOverflow", ...
    details: String,        // human readable explanation with hex addresses
    fault_address_mapped: bool | null,
    fault_address_mapping: {
        path: String | null,
        permissions: String,
        offset_in_mapping: String,
    } | null,

    crash_location: {
        path: String | null,
        permissions: String,
        offset_in_mapping: String,
    } | null,

    stack_pointer_valid: bool | null,
    null_registers: [String] | null,  // ["rax", "rcx"] registers with near-null values
}
```

All fields except `summary` and `category` are optional, allowing partial diagnosis when not all inputs are available.

### Example Diagnosis Payloads

**Null pointer dereference:**
```json
{
  "diagnosis": {
    "summary": "Null pointer dereference",
    "category": "NullPointerDereference",
    "details": "SIGSEGV (SEGV_MAPERR) at address 0x0000000000000000. Address is below the null-page threshold (0x10000), suggesting a direct null dereference on a null pointer. Crash in /home/bits/go/src/github.com/DataDog/libdatadog/target/release/crashtracker_bin_test.",
    "fault_address_mapped": false,
    "crash_location": {
      "path": "/home/bits/go/src/github.com/DataDog/libdatadog/target/release/crashtracker_bin_test",
      "permissions": "r-xp",
      "offset_in_mapping": "0x29f57"
    },
    "stack_pointer_valid": true,
    "null_registers": ["r10", "r14", "r15", "rax", "rcx", "rdi"]
  }
}
```

**Stack overflow:**
```json
{
    "summary": "Stack overflow",
    "category": "StackOverflow",
    "details": "SIGSEGV (SEGV_MAPERR) at address 0x00007ffc95ff8818. Fault address is near the stack guard page (stack mapping: 0x7ffc95ff9000-0x7ffc967f9000, SP=0x00007ffc95ff8818). Stack exhaustion detected.",
    "fault_address_mapped": false,
    "crash_location": {
        "path": "/home/bits/go/src/github.com/DataDog/libdatadog/target/release/crashtracker_bin_test",
        "permissions": "r-xp",
        "offset_in_mapping": "0x2a520"
    },
    "stack_pointer_valid": false,
    "null_registers": ["r10", "r13", "r15", "rbp", "rbx", "rcx", "rdi", "rsi"]
}
```

## Implementation

### Where Should Diagnosis Happen?

**Recommendation:** EVP errors processing pipeline.

**Reasons:**
- Doing diagnosis and enrichment work outside of the crashing customer app
- Changes to diagnosis logic are reflected immediately with backend updates
- Processing and enrichment will be in a step that is specifically for error logs
- Avoids any performance impact on the signal handler or receiver

**Trade-offs:**
- Cannot enrich telemetry intake logs (in the Logs view)
- Diagnosis is not available immediately to all delivery paths

This choice means that logs viewed in the Logs UI for crashes will not include the diagnosis. However, the goal of crashtracking is to surface crashes in the Errors UI as the primary source for viewing crashes.
