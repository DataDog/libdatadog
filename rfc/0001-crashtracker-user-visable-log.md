# RFC 0001: Crashtracker Structured Log Format

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED",  "MAY", and "OPTIONAL" in this document are to be interpreted as described in [IETF RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

## Summary

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
- `incomplete`: Boolean `false` if the crashreport is complete (i.e. contains all intended data), `true` if there is important missing data (e.g. the crashtracker itself crashed during stack trace collection).
    This MUST be set to `true` if any required field is missing.
- [TODO] there should probably be an errortype:crash field?
- os_info: The architecture on which the 
- stacktrace: This represents the stack 
- timestamp: The time at which the crash occurred, in ISO 8601 format.
- uuid: A UUID which uniquely identifies the crash.
- version_id: A Semver compatible ID for this format. [TODO, should it be semver?]

### Optional fields
Any field not listed as "Required" is optional.
In order to minimize logging overhead, producers SHOULD NOT emit anything for an optional field.
Consumers MUST accept json with elided optional fields.

- additional_stacktraces: This field contains a `Map<ThreadId, Stacktrace>`.
    In a multi-threaded program, the collector SHOULD collect the stacktraces of all active threads, and report them here.
- counters: The crashtracker offers a mechanism for programs to register counters which track the state of the system.
    At present, 
- files
- metadata: Option<CrashtrackerMetadata>,
- proc_info: Currently, just tracks the PID of the crashing process.  
             In the future, this may record additional info about the crashing process.
- siginfo: The name and 
- span_ids: Vec<u128>,
    pub trace_ids: Vec<u128>,
    pub tags: HashMap<String, String>,

### Extensibility


### Stacktraces
Different languages and language runtimes have different representations of a stacktrace.
The representation below attempts to collect as much information.
In addition, not all information may be available at crash-time on a given machine.
For example, some libraries may have been shipped with debug symbols stripped, meaning that the only information available about a given frame may be the instruction pointer (`ip`) address, stored as a hex number "0xDEADBEEF".
This address may be given as an absolute address, or a `NormalizedAddress`, which can be used by backend symbolication.
We follow the [blazezym](https://github.com/libbpf/blazesym) format for normalized addresses.
For frames where debug information is available, this information is stored in an array of `StackFrameNames`.
Note that an array is necessary, since a single assembly level instruction may correspond to multiple code locations (e.g. in the case of inlined functions).

A stack frame can be represented as the following `json` schema, whose `rust` implementation is given in the appendix:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "StackFrame",
  "description": "All fields are hex encoded integers.",
  "type": "object",
  "properties": {
    "ip": {
      "type": [
        "string",
        "null"
      ]
    },
    "module_base_address": {
      "type": [
        "string",
        "null"
      ]
    },
    "names": {
      "type": [
        "array",
        "null"
      ],
      "items": {
        "$ref": "#/definitions/StackFrameNames"
      }
    },
    "normalized_ip": {
      "anyOf": [
        {
          "$ref": "#/definitions/NormalizedAddress"
        },
        {
          "type": "null"
        }
      ]
    },
    "sp": {
      "type": [
        "string",
        "null"
      ]
    },
    "symbol_address": {
      "type": [
        "string",
        "null"
      ]
    }
  },
  "definitions": {
    "NormalizedAddress": {
      "type": "object",
      "required": [
        "file_offset",
        "meta"
      ],
      "properties": {
        "file_offset": {
          "type": "integer",
          "format": "uint64",
          "minimum": 0.0
        },
        "meta": {
          "$ref": "#/definitions/NormalizedAddressMeta"
        }
      }
    },
    "NormalizedAddressMeta": {
      "oneOf": [
        {
          "type": "string",
          "enum": [
            "Unknown"
          ]
        },
        {
          "type": "object",
          "required": [
            "Apk"
          ],
          "properties": {
            "Apk": {
              "type": "string"
            }
          },
          "additionalProperties": false
        },
        {
          "type": "object",
          "required": [
            "Elf"
          ],
          "properties": {
            "Elf": {
              "type": "object",
              "required": [
                "path"
              ],
              "properties": {
                "build_id": {
                  "type": [
                    "array",
                    "null"
                  ],
                  "items": {
                    "type": "integer",
                    "format": "uint8",
                    "minimum": 0.0
                  }
                },
                "path": {
                  "type": "string"
                }
              }
            }
          },
          "additionalProperties": false
        },
        {
          "type": "object",
          "required": [
            "Unexpected"
          ],
          "properties": {
            "Unexpected": {
              "type": "string"
            }
          },
          "additionalProperties": false
        }
      ]
    },
    "StackFrameNames": {
      "type": "object",
      "properties": {
        "colno": {
          "type": [
            "integer",
            "null"
          ],
          "format": "uint32",
          "minimum": 0.0
        },
        "filename": {
          "type": [
            "string",
            "null"
          ]
        },
        "lineno": {
          "type": [
            "integer",
            "null"
          ],
          "format": "uint32",
          "minimum": 0.0
        },
        "name": {
          "type": [
            "string",
            "null"
          ]
        }
      }
    }
  }
}
```

### Other data

## Appendix A: Example output
```json
{
  "counters": {
    "unwinding": 0,
    "not_profiling": 0,
    "serializing": 1,
    "collecting_sample": 0
  },
  "incomplete": false,
  "metadata": {
    "profiling_library_name": "crashtracking-test",
    "profiling_library_version": "12.34.56",
    "family": "crashtracking-test",
    "tags": []
  },
  "os_info": {
    "os_type": "Macos",
    "version": {
      "Semantic": [
        14,
        5,
        0
      ]
    },
    "edition": null,
    "codename": null,
    "bitness": "X64",
    "architecture": "arm64"
  },
  "proc_info": {
    "pid": 95565
  },
  "siginfo": {
    "signum": 11,
    "signame": "SIGSEGV"
  },
  "span_ids": [
    42
  ],
  "stacktrace": [
    {
      "ip": "0x100f702ac",
      "names": [
        {
          "colno": 5,
          "filename":
"/Users/daniel.schwartznarbonne/.cargo/registry/src/index.crates.io-6f17d22bba15001f/backtrace-0.3.71/src/backtrace/libunwind.rs",
          "lineno": 105,
          "name": "trace"
        },
        {
          "colno": 5,
          "filename":
"/Users/daniel.schwartznarbonne/.cargo/registry/src/index.crates.io-6f17d22bba15001f/backtrace-0.3.71/src/backtrace/mod.rs",
          "lineno": 66,
          "name":
"trace_unsynchronized<datadog_crashtracker::collectors::emit_backtrace_by_frames::{closure_env#0}<std::process::ChildStdin>>"
        },
        {
          "colno": 5,
          "filename":
"/Users/daniel.schwartznarbonne/go/src/github.com/DataDog/libdatadog/crashtracker/src/collectors.rs",
          "lineno": 33,
          "name": "emit_backtrace_by_frames<std::process::ChildStdin>"
        }
      ],
      "sp": "0x16f9658c0",
      "symbol_address": "0x100f702ac"
    },
    {
      "ip": "0x100f6f518",
      "names": [
        {
          "colno": 18,
          "filename":
"/Users/daniel.schwartznarbonne/go/src/github.com/DataDog/libdatadog/crashtracker/src/crash_handler.rs",
          "lineno": 379,
          "name": "emit_crashreport<std::process::ChildStdin>"
        },
        {
          "colno": 23,
          "filename":
"/Users/daniel.schwartznarbonne/go/src/github.com/DataDog/libdatadog/crashtracker/src/crash_handler.rs",
          "lineno": 414,
          "name": "handle_posix_signal_impl"
        },
        {
          "colno": 13,
          "filename":
"/Users/daniel.schwartznarbonne/go/src/github.com/DataDog/libdatadog/crashtracker/src/crash_handler.rs",
          "lineno": 264,
          "name": "handle_posix_sigaction"
        }
      ],
      "sp": "0x16f965940",
      "symbol_address": "0x100f6f518"
    },
    {
      "ip": "0x186b9b584",
      "names": [
        {
          "name": "__simple_esappend"
        }
      ],
      "sp": "0x16f965ae0",
      "symbol_address": "0x186b9b584"
    },
    {
      "ip": "0x10049bd94",
      "names": [
        {
          "name": "_main"
        }
      ],
      "sp": "0x16f965b10",
      "symbol_address": "0x10049bd94"
    }
  ],
  "trace_ids": [
    18446744073709551617
  ],
  "timestamp": "2024-07-19T16:52:16.422378Z",
  "uuid": "a42add90-0e60-4799-b9f7-cbe0ebec4f27"
}
```

## Appendix B: Rust implementation of stacktrace format

## Appendic C: Schema for the entire json thing as it stands