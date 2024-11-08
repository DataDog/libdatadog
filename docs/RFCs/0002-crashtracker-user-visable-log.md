# RFC 0002: Crashtracker Structured Log Format (Version 1.0)

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in [IETF RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

## Summary

This document describes version 1.0 of the crashinfo data format.

## Motivation

The `libdatadog` crashtracker detects program crashes.
It automatically collects information relevant to the characterizing and debugging the crash, including stack-traces, the crash-type (e.g. SIGSIGV, SIGBUS, etc) crash, the library version, etc.
This RFC establishes a standardized logging format for reporting this information.

### Why structured json

As a text-based format, json can be written to standard logging endpoints.
It is (somewhat) human readable, so users can directly interpret the crash info off their log if necessary.
As a structured format, it avoids the ambiguity of standard semi-structured stacktrace formats (as used by e.g. Java, .Net, etc).
Due to the use of native extensions, it is possible for a single stack-trace to include frames from multiple languages (e.g. python may call C code, which calls Rust code, etc).
Having a single structured format allows us to work across languages.

## Proposed format

A natural language description of the proposed json format is given here.
An example is given in Appendix A, and the schema is given in Appendix B.

### Required fields

- `data_schema_version`:
  A string containing the semver ID of the crashtracker data schema ("1.0" for the current version).
- `error`:
  A structure of type `ErrorData`, described below.
- `incomplete`:
  Boolean `false` if the crashreport is complete (i.e. contains all intended data), `true` if there is expected missing data.
  This can happen becasue the crashtracker is architected to stream data to an out of process receiver, allowing a partial crash report to be emitted even in the case where the crashtracker itself crashed during stack trace collection.
  This MUST be set to `true` if any required field is missing.
- `metadata`:
  Metadata about the system in which the crash occurred:
  - `library_name`:
    e.g. "dd-trace-python".
  - `library_version`:
    e.g. "2.16.0".
  - `family`:
    e.g. "python".
  - `tags`:
    A set of key:value pairs, representing any tags the crashtracking system wishes to associate with the crash.
    Examples would include "hostname", "service", and any configuration information the system wishes to track.
- `os_info`:
  The OS + processor architecture on which the crash occurred.
  - `architecture`
  - `bitness`
  - `os_type`
  - `version`
- `timestamp`:
  The time at which the crash occurred, in ISO 8601 format.
- `uuid`:
  A UUID v4 which uniquely identifies the crash.
  This will typically be generated at crash-time, and then associated with the uploaded crash.

### ErrorData

- `threads`:
  This **optional** field contains an array of `Thread` objects.
  In a multi-threaded program, the collector SHOULD collect the stacktraces of all active threads, and report them here.
  A `Thread` object has the following fields:
  - `crashed`: a boolean which tells if the thread crashed.
  - `name`: Name of the thread (e.g. 'Thread 0').
  - `stack`: The `StackTrace` of the thread
  - `state`: **Optional**. Platform-specific state of the thread when its state was captured (CPU registers dump for iOS, thread state enum for Android, etc.).
    See below for more details on how stacktraces are formatted.
- `is_crash`:
  Boolean true if the error was a crash, false otherwise.
- `kind`:
  The kind of error that occurred.
  For a crash, a non-exhaustive set of options include "SigAbort", "SigBus", "SigSegv".
  This field MAY be extended to include options such at "Panic", "UnhandledException" etc.
- `message`:
  A human readable string containing an error message associated with the stack trace.
- `source_type`:
  The string "crashtracking".
- `stack`:
  This represents the stack of the crashing thread.
  See below for more details on how stacktraces are formatted.

### Optional fields

Any field not listed as "Required" is optional.
In order to minimize logging overhead, producers SHOULD NOT emit anything for an optional field.
Consumers MUST accept json with elided optional fields.

- `counters`:
  The crashtracker offers a mechanism for programs to register counters to track which operations were active at the time of the crash.
  At present, this is only used by the profiler, but this may be extended in the future.
- `files`:
  The collector MAY collect useful files, such as `/proc/self/maps` or `/proc/meminfo`, and include them here.
  Files are stored as a `Map<filename, contents>` where `contents` is an array of plain text strings, one per line.
- `proc_info`:
  Currently, just tracks the PID of the crashing process.  
   In the future, this may record additional info about the crashing process.
- `sig_info`:
  UNIX only: Useful information from the [siginfo_t](https://man7.org/linux/man-pages/man2/sigaction.2.html) structure.
  - `faulting_address`: An **optional** hexidecimal string with the memory address at which the fault occurred, e.g. "0xdeadbeef".
  - `signame`: The signal name, e.g. "SIGSEGV".
  - `signum`: An integer storing the [UNIX signal number](https://man7.org/linux/man-pages/man7/signal.7.html), e.g. `11` for a segmentation violation.
- `span_ids`:
  A vector of string identifiers, representing the active span ids at the time of program crash.
  The collector SHOULD collect as many as it can, but MAY cap the number of spans that it tracks.
- `trace_ids:`
  A vector of string identifiers, representing the active span ids at the time of program crash.
  The collector SHOULD collect as many as it can, but MAY cap the number of spans that it tracks.

### Extensibility

Future versions of the crashtracker MAY add additional fields.
Parsers MUST accept unexpected optional fields, either by ignoring them, or by displaying them as additional data.
The version number SHOULD follow semver: i.e. increment a minor version number for backwards compatable changes, and increment the major version for non backwards compatible changes.

### Stacktraces

Different languages and language runtimes have different representations of a stacktrace.
The representation below attempts to collect as much information as possible.
In addition, not all information may be available at crash-time on a given machine.
For example, some libraries may have been shipped with debug symbols stripped, meaning that the only information available about a given frame may be the instruction pointer (`ip`) address, stored as a hex number "0xDEADBEEF".
This address may be given as an absolute address, or a `NormalizedAddress`, which can be used by backend symbolication.
We follow the [blazezym](https://github.com/libbpf/blazesym) format for normalized addresses.

A stacktrace consists of

- `format`:
  An identifier describing the format of the stack trace.
  Allows for extensibility to support different stack trace formats.
  The format described below is identfied using the string "CrashTrackerV1"
- `frames`:
  An array of `StackFrame`, described below.
  Note that each inlined function gets its own stack frame in this schema.

#### StackFrame

NOTE: All of the given fields below are optional.

- **Absolute Addresses**
  The actual in-memory addresses used in the crashing process.
  Combined with mapping information, such as from `/proc/self/maps`, and the relevant binaries, this can be used to reconstruct relevant symbols.
  These fields follow the scheme used by the [backtrace crate](https://docs.rs/backtrace/latest/backtrace/struct.Frame.html)
  - `ip`:
    The current instruction pointer of this frame.
    This is normally the next instruction to execute in the frame, but not all implementations list this with 100% accuracy (but it’s generally pretty close).
  - `sp`:
    The current stack pointer of this frame.
  - `symbol_address`:
    The starting symbol address of the frame of this function.
    This will attempt to rewind the instruction pointer returned by ip to the start of the function, returning that value.
    In some cases, however, backends will just return ip from this function.
  - `module_base_address`:
    The base address of the module to which the frame belongs
- **Relative Addresses**
  Addresses expressed as an offset into a given library or executable.
  Can be used by backend symbolication to generate debug names etc.
  - `build_id`:
    A string identifying the build id of the module the address belongs to.
    For example, GNU build ids are hex strings "9944168df12b0b9b152113c4ad663bc27797fb15".
    Pdb build ids can be stored as a concatenation of the guid and the age (using a well-known separator).
  - `build_id_type`:
    The type of the `build_id`. E.g. "SHA1/GNU/GO/PDB/PE".
  - `file_type`: The file type of the module containing the symbol, e.g. "ELF", "PDB", etc.
  - `relative_address`: The relative offset of the symbol in the base file, given as a hexidecimal string.
  - `path`: The path to the module containing the symbol.
- **Debug information (e.g. "names")**
  Human readable debug information representing the location of the stack frame in the high-level code.
  Note that this is a best effort collection: for optimized code, it may be difficult to associate a given instruction back to file, line and column.
  - `column`:
    The column number in the given file where the symbol was defined.
  - `file`:
    The file name where this function was defined.
  - `line`
    The line number in the given file where the symbol was defined.
  - `function`
    The name of the function.
    This may or may not include module information.
    It may or may not be demangled (e.g. "\_ZNSt28**atomic_futex_unsigned_base26_M_futex_wait_until_steadyEPjjbNSt6chrono8durationIlSt5ratioILl1ELl1EEEENS2_IlS3_ILl1ELl1000000000EEEE" vs "std::**atomic_futex_unsigned_base::\_M_futex_wait_until_steady")

### Other data

## Appendix A: Example output

[Available here](artifacts/0002-crashtracker-example.json)

## Appendix B: Json Schema

[Available here](artifacts/0002-crashtracker-schema.json)
