# RFC 0011: v1.X Crashtracker Structured Log Format

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in [IETF RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

## Summary

This document consolidates and describes the complete evolution of the crashinfo data format from version 1.0 through 1.4. It serves as the authoritative specification for the crashtracker structured log format, replacing RFCs 0005-0009. Future minor version modifications will be included in this revisable document.

## Motivation

The `libdatadog` crashtracker detects program crashes and automatically collects information relevant to characterizing and debugging the crash, including stack-traces, crash-type (e.g. SIGSEGV, SIGBUS, etc), library version, etc. This RFC consolidates the standardized logging format that has evolved through multiple iterations to support enhanced debugging capabilities.

### Why structured json

As a text-based format, json can be written to standard logging endpoints.
It is (somewhat) human readable, so users can directly interpret the crash info off their log if necessary.
As a structured format, it avoids the ambiguity of standard semi-structured stacktrace formats (as used by e.g. Java, .Net, etc).
Due to the use of native extensions, it is possible for a single stack-trace to include frames from multiple languages (e.g. python may call C code, which calls Rust code, etc).
Having a single structured format allows us to work across languages.

## Current Format (Version 1.4)

This section describes the current format (version 1.4), which incorporates all features from versions 1.0 through 1.4. A natural language description of the json format is given here. An example is given in Appendix A, and the schema is given in Appendix B.

Any field not listed as "Required" is optional. Consumers MUST accept json with elided optional fields.

### Extensibility

The data-format has a REQUIRED `data_schema_version` field, which represents the semver version ID of the data.
Following semver, collectors may add additional fields without affecting the major version number.
Parsers SHOULD therefore accept unexpected fields, either by ignoring them, or by displaying them as additional data.

### Version Compatibility

Consumers of the crash data format SHOULD be designed to handle all versions from 1.0 to 1.4. The version is indicated by the `data_schema_version` field. Key compatibility considerations:
- Version 1.0: Base format
- Version 1.1+: Stacktraces may include an `incomplete` field
- Version 1.2+: Root level may include an `experimental` field
- Version 1.3+: Stackframes may include a `comments` field
- Version 1.4+: Stackframes may include a `mangled_name` field

### Fields

- `counters`: **[optional]**
  A map of names to integer values.
  At present, this is used by the profiler to track which operations were active at the time of the crash.
- `data_schema_version`: **[required]**
  A string containing the semver ID of the crashtracker data schema. Current versions: "1.0", "1.1", "1.2", "1.3", "1.4".
- `experimental`: **[optional]** *[Added in v1.2]*
  Any valid JSON object can be used as the value here.
  Note that the object MUST be valid JSON.
  Consumers of the format SHOULD pass this field along unmodified as the report is processed.
  This field allows developers to collect experimental data without requiring schema changes.
- `error`: **[required]**
  - `threads`: **[optional]**
    An array of `Thread` objects.
    In a multi-threaded program, the collector SHOULD collect the stacktraces of all active threads, and report them here.
    A `Thread` object has the following fields:
    - `crashed`: **[required]**
      A boolean which tells if the thread crashed.
    - `name`: **[required]**
      Name of the thread (e.g. 'Thread 0').
    - `stack`: **[required]**
      The `StackTrace` of the thread.
      See below for more details on how stacktraces are formatted.
    - `state`: **[optional]**
      Platform-specific state of the thread when its state was captured (CPU registers dump for iOS, thread state enum for Android, etc.).
      Currently, this is a platform-dependent string.
  - `is_crash`: **[required]**
    Boolean true if the error was a crash, false otherwise.
  - `kind`: **[required]**
    The kind of error that occurred.
    For example, "Panic", "UnhandledException", "UnixSignal".
  - `message`: **[optional]**
    A human readable string containing an error message associated with the stack trace.
  - `source_type`: **[required]**
    The string "Crashtracking".
  - `stack`: **[required]**
    This represents the stack of the crashing thread.
    See below for more details on how stacktraces are formatted.
- `files`: **[optional]**
  A `Map<filename, contents>` where `contents` is an array of plain text strings, one per line.
  Useful files for triage and debugging, such as `/proc/self/maps` or `/proc/meminfo`.
- `fingerprint`: **[optional]**
  A string containing a summary or hash of crash information which can be used for deduplication.
- `incomplete`: **[required]**
  Boolean `false` if the crashreport is complete (i.e. contains all intended data), `true` if there is expected missing data.
  This can happen becasue the crashtracker is architected to stream data to an out of process receiver, allowing a partial crash report to be emitted even in the case where the crashtracker itself crashed during stack trace collection.
  This MUST be set to `true` if any required field is missing.
- `log_messages`: **[optional]**
  An array of strings containing log messages generated by the crashtracker.
- `metadata`: **[required]**
  Metadata about the system in which the crash occurred:
  - `library_name`: **[required]**
    e.g. "dd-trace-python".
  - `library_version`: **[required]**
    e.g. "2.16.0".
  - `family`: **[required]**
    e.g. "python".
  - `tags`: **[optional]**
    A set of key:value pairs, representing any tags the crashtracking system wishes to associate with the crash.
    Examples would include "hostname", "service", and any configuration information the system wishes to track.
- `os_info`: **[required]**
  The OS + processor architecture on which the crash occurred.
  Follows the display format of the [os_info crate](https://crates.io/crates/os_info).
  - `architecture`: **[required]**
    e.g. "arm64"
  - `bitness`: **[required]**
    e.g. "64-bit".
  - `os_type`: **[required]**
    e.g. "Mac OS".
  - `version`: **[required]**
    e.g. "14.7.0".
- `proc_info`: **[optional]**
  A place to store information about the crashing process.
  In the future, this may have additional optional fields as more data is collected.
  - `pid`: **[required]**
    The PID of the crashing process.
- `sig_info`: **[optional]**
  UNIX signal based collectors only: Useful information from the [siginfo_t](https://man7.org/linux/man-pages/man2/sigaction.2.html) structure.
  - `sid_addr`: **[optional]**
    A hexidecimal string with the memory address at which the fault occurred, e.g. "0xDEADBEEF".
  - `si_code`: **[required]**
    An integer storing the [UNIX signal code](https://man7.org/linux/man-pages/man7/signal.7.html), e.g. `1` for a `SEGV_MAPERR`.
  - `si_code_human_readable`: **[required]**
    The signal code expressed as a human readable string, e.g. "SEGV_MAPERR" for `SEGV_MAPERR`.
    Follows the naming convention in [the manpage](https://man7.org/linux/man-pages/man7/signal.7.html).
  - `si_signo`: **[required]**
    An integer storing the [UNIX signal number](https://man7.org/linux/man-pages/man7/signal.7.html), e.g. `11` for a segmentation violation.
  - `si_signo_human_readable`: **[required]**
    The signal name, e.g. "SIGSEGV".
    Follows the naming convention in [the manpage](https://man7.org/linux/man-pages/man7/signal.7.html).
- `span_ids`: **[optional]**
  A vector representing active span ids at the time of program crash.
  The collector MAY cap the number of spans that it tracks.
  - `id`: **[required]**
    A string containing the span id.
  - `thread_name`: **[optional]**
    A string containing the thread name for the given span.
- `timestamp`: **[required]**
  The time at which the crash occurred, in ISO 8601 format.
- `trace_ids:`: **[optional]**
  A vector representing active span ids at the time of program crash.
  The collector MAY cap the number of spans that it tracks.
  - `id`: **[required]**
    A string containing the trace id.
  - `thread name`: **[optional]**
    A string containing the thread name for the given trace.
- `uuid`: **[required]**
  A UUID v4 which uniquely identifies the crash.
  This will typically be generated at crash-time, and then associated with the uploaded crash.

### Stacktraces

Different languages and language runtimes have different representations of a stacktrace.
The representation below attempts to collect as much information as possible.
In addition, not all information may be available at crash-time on a given machine.
For example, some libraries may have been shipped with debug symbols stripped, meaning that the only information available about a given frame may be the instruction pointer (`ip`) address, stored as a hex number "0xDEADBEEF".
This address may be given as an absolute address, or a `NormalizedAddress`, which can be used by backend symbolication.

A stacktrace consists of

- `format`: **[required]**
  An identifier describing the format of the stack trace.
  Allows for extensibility to support different stack trace formats.
  The format described below is identified using the string "Datadog Crashtracker 1.0"
- `frames`: **[required]**
  An array of `StackFrame`, described below.
  Note that each inlined function gets its own stack frame in this schema.
- `incomplete`: **[optional]** *[Added in v1.1]*
  A boolean denoting whether the stacktrace may be missing frames, either due to intentional truncation, or an inability to fully collect a corrupted stack.

#### StackFrames

- **Absolute Addresses**
  The actual in-memory addresses used in the crashing process.
  Combined with mapping information, such as from `/proc/self/maps`, and the relevant binaries, this can be used to reconstruct relevant symbols.
  These fields follow the scheme used by the [backtrace crate](https://docs.rs/backtrace/latest/backtrace/struct.Frame.html)
  - `ip`: **[optional]**
    The current instruction pointer of this frame.
    This is normally the next instruction to execute in the frame, but not all implementations list this with 100% accuracy (but it’s generally pretty close).
  - `sp`: **[optional]**
    The current stack pointer of this frame.
  - `symbol_address`: **[optional]**
    The starting symbol address of the frame of this function.
    This will attempt to rewind the instruction pointer returned by ip to the start of the function, returning that value.
    In some cases, however, backends will just return ip from this function.
  - `module_base_address`: **[optional]**
    The base address of the module to which the frame belongs
- **Relative Addresses**
  Addresses expressed as an offset into a given library or executable.
  Can be used by backend symbolication to generate debug names etc.
  Note that tracking this per stack frame can entail significant duplication of information.
  Adding a "modules" section and referencing it by index, as in the pprof specification, is future work.
  - `build_id`: **[optional]**
    A string identifying the build id of the module the address belongs to.
    For example, GNU build ids are hex strings "9944168df12b0b9b152113c4ad663bc27797fb15".
    Pdb build ids can be stored as a concatenation of the guid and the age (using a well-known separator).
  - `build_id_type`: **[required if `build_id` is set, optional otherwise]**
    The type of the `build_id`. E.g. "SHA1/GNU/GO/PDB/PE".
  - `file_type`: **[required if `relative_address` is set, optional otherwise]**
    The file type of the module containing the symbol, e.g. "ELF", "PDB", etc.
  - `relative_address`: **[optional]**
    The relative offset of the symbol in the base file (e.g. an ELF virtual address), given as a hexidecimal string.
  - `path`: **[required if `relative_address` is set, optional otherwise]**
    The path to the module containing the symbol.
- **Debug information (e.g. "names")**
  Human readable debug information representing the location of the stack frame in the high-level code.
  Note that this is a best effort collection: for optimized code, it may be difficult to associate a given instruction back to file, line and column.
  - `column`: **[optional]**
    The column number in the given file where the symbol was defined.
  - `file`: **[optional]**
    The file name where this function was defined.
    Note that this may be either an absolute or relative path.
  - `line`: **[optional]**
    The line number in the given file where the symbol was defined.
  - `function`: **[optional]**
    The name of the function.
    This may or may not include module information.
    It may or may not be demangled (e.g. "\_ZNSt28**atomic_futex_unsigned_base26_M_futex_wait_until_steadyEPjjbNSt6chrono8durationIlSt5ratioILl1ELl1EEEENS2_IlS3_ILl1ELl1000000000EEEE" vs "std::**atomic_futex_unsigned_base::\_M_futex_wait_until_steady")
  - `comments`: **[optional]** *[Added in v1.3]*
    An array of strings containing comments about the given stackframe.
    For example, if a stackframe failed to symbolicate, the crashtracker implementation may record the reason for the failure.
  - `mangled_name`: **[optional]** *[Added in v1.4]*
    A string containing the original mangled name of the function, if the function name was demangled.
    This field is only present when the function name has been demangled and the original mangled name differs from the demangled name.

## Version History

This section documents the evolution of the crashtracker structured log format across versions 1.0 through 1.4. The current specification above reflects version 1.4, which includes all features from previous versions.

### Version 1.0 (RFC 0005)
*Initial version*

- Established the base JSON schema for crash reporting
- Defined core fields: `counters`, `data_schema_version`, `error`, `files`, `fingerprint`, `incomplete`, `log_messages`, `metadata`, `os_info`, `proc_info`, `sig_info`, `span_ids`, `timestamp`, `trace_ids`, `uuid`
- Defined stacktrace format with `format` and `frames` fields
- Defined comprehensive stackframe schema with absolute addresses, relative addresses, and debug information

### Version 1.1 (RFC 0006)
*Added incomplete stacktraces*

**Changes from v1.0:**
- Added `incomplete` field to `StackTrace` objects (optional boolean)
- Updated `data_schema_version` to "1.1"

**Motivation:** Some stacktraces may be incomplete due to intentional truncation for performance reasons or unintentional failure (e.g., corrupted stack). The `incomplete` flag allows consumers to know that frames may be missing.

### Version 1.2 (RFC 0007)
*Added experimental field*

**Changes from v1.1:**
- Added `experimental` field at root level (optional JSON object)
- Updated `data_schema_version` to "1.2"

**Motivation:** Developers may wish to collect experimental data without requiring frequent schema changes. The `experimental` field allows ad-hoc data collection that can later be promoted to structured fields once proven valuable.

### Version 1.3 (RFC 0008)
*Added stackframe comments*

**Changes from v1.2:**
- Added `comments` field to `StackFrame` objects (optional array of strings)
- Updated `data_schema_version` to "1.3"

**Motivation:** Crashtracker implementations may have additional information about stackframes that doesn't fit the current schema (e.g., symbolication failure reasons). Comments provide a way to record this information for debugging purposes.

### Version 1.4 (RFC 0009)
*Added stackframe mangled names*

**Changes from v1.3:**
- Added `mangled_name` field to `StackFrame` objects (optional string)
- Updated `data_schema_version` to "1.4"

**Motivation:** When symbol names are demangled for readability, the original mangled names are lost. This makes debugging difficult when mangled names are needed (e.g., comparing against compiler-generated symbols). The `mangled_name` field preserves the original mangled name when demangling occurs.

## Appendix A: Example output

An example crash report in version 1.0 format is [available here](artifacts/0005-crashtracker-example.json).

Note: This example uses version 1.0 format. Version 1.1+ may include additional fields such as `incomplete` in stacktraces, `experimental` at the root level, `comments` in stackframes, and `mangled_name` in stackframes.

## Appendix B: Json Schema

The current JSON schema (version 1.4) is [available here](artifacts/0009-crashtracker-schema.json).

Historical schemas are also available:
- [Version 1.0 schema](artifacts/0005-crashtracker-schema.json)
- [Version 1.1 schema](artifacts/0006-crashtracker-schema.json)
- [Version 1.2 schema](artifacts/0007-crashtracker-schema.json)
- [Version 1.3 schema](artifacts/0008-crashtracker-schema.json)
- [Version 1.4 schema](artifacts/0009-crashtracker-schema.json)
