# RFC 0001: Crashtracker Structured Log Format

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
A single structured format 

## Proposed format

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

```rust

```

### Other data

## Appendix A: Example output

## Appendix B: Rust implementation of stacktrace format