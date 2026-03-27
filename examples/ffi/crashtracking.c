// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/common.h>
#include <datadog/crashtracker.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define MAX_FILE_PATH   512
#define INIT_FROM_SLICE(s)                                                                         \
  { .ptr = s.ptr, .len = s.len }

static ddog_CharSlice slice(const char *s) { return (ddog_CharSlice){.ptr = s, .len = strlen(s)}; }

void handle_result(ddog_VoidResult result) {
  if (result.tag == DDOG_VOID_RESULT_ERR) {
    ddog_CharSlice message = ddog_Error_message(&result.err);
    fprintf(stderr, "%.*s\n", (int)message.len, message.ptr);
    ddog_Error_drop(&result.err);
    exit(EXIT_FAILURE);
  }
}

uintptr_t handle_uintptr_t_result(ddog_crasht_Result_Usize result) {
  if (result.tag == DDOG_CRASHT_RESULT_USIZE_ERR_USIZE) {
    ddog_CharSlice message = ddog_Error_message(&result.err);
    fprintf(stderr, "%.*s\n", (int)message.len, message.ptr);
    ddog_Error_drop(&result.err);
    exit(EXIT_FAILURE);
  }
  return result.ok;
}

int main(int argc, char **argv) {
  // Receiver binary path: CLI arg > env var > hardcoded default
  const char *receiver_path = NULL;
  if (argc >= 2) {
    receiver_path = argv[1];
  } else {
    receiver_path = getenv("DDOG_CRASHT_TEST_RECEIVER");
  }
  if (!receiver_path || receiver_path[0] == '\0') {
    receiver_path = "/tmp/libdatadog/bin/libdatadog-crashtracking-receiver";
  }

  // Output directory: env var > hardcoded default
  const char *output_dir = getenv("DDOG_CRASHT_TEST_OUTPUT_DIR");
  if (!output_dir || output_dir[0] == '\0') {
    output_dir = "/tmp/crashreports";
  }

  // Build output file paths
  char report_path[MAX_FILE_PATH];
  char stderr_path[MAX_FILE_PATH];
  char stdout_path[MAX_FILE_PATH];
  snprintf(report_path, sizeof(report_path), "file://%s/crashreport.json", output_dir);
  snprintf(stderr_path, sizeof(stderr_path), "%s/stderr.txt", output_dir);
  snprintf(stdout_path, sizeof(stdout_path), "%s/stdout.txt", output_dir);

  // Forward the dynamic-linker search path to the receiver process.
#ifdef __APPLE__
  const char *ld_search_path_var = "DYLD_LIBRARY_PATH";
#else
  const char *ld_search_path_var = "LD_LIBRARY_PATH";
#endif
  const char *ld_library_path = getenv(ld_search_path_var);
  ddog_crasht_EnvVar env_vars[1];
  ddog_crasht_Slice_EnvVar env_slice = {.ptr = NULL, .len = 0};
  if (ld_library_path && ld_library_path[0] != '\0') {
    env_vars[0].key = slice(ld_search_path_var);
    env_vars[0].val = slice(ld_library_path);
    env_slice.ptr = env_vars;
    env_slice.len = 1;
  }

  ddog_crasht_ReceiverConfig receiver_config = {
      .args = {},
      .env = env_slice,
      .path_to_receiver_binary = slice(receiver_path),
      .optional_stderr_filename = slice(stderr_path),
      .optional_stdout_filename = slice(stdout_path),
  };

  // Get the default signals and explicitly use them.
  struct ddog_crasht_Slice_CInt signals = ddog_crasht_default_signals();
  ddog_crasht_Config config = {
      .create_alt_stack = false,
      .endpoint = {.url = slice(report_path)},
      .resolve_frames = DDOG_CRASHT_STACKTRACE_COLLECTION_ENABLED_WITH_INPROCESS_SYMBOLS,
      .signals = INIT_FROM_SLICE(signals),
  };

  ddog_crasht_Metadata metadata = {
      .library_name = DDOG_CHARSLICE_C("crashtracking-test"),
      .library_version = DDOG_CHARSLICE_C("12.34.56"),
      .family = DDOG_CHARSLICE_C("crashtracking-test"),
      .tags = NULL,
  };

  handle_result(ddog_crasht_init(config, receiver_config, metadata));

  handle_result(ddog_crasht_begin_op(DDOG_CRASHT_OP_TYPES_PROFILER_COLLECTING_SAMPLE));
  handle_uintptr_t_result(ddog_crasht_insert_span_id(0, 42));
  handle_uintptr_t_result(ddog_crasht_insert_trace_id(1, 1));
  handle_uintptr_t_result(ddog_crasht_insert_additional_tag(
      DDOG_CHARSLICE_C("This is a very informative extra bit of info")));
  handle_uintptr_t_result(ddog_crasht_insert_additional_tag(
      DDOG_CHARSLICE_C("This message will for sure help us debug the crash")));

#ifdef EXPLICIT_RAISE_SEGV
  // Test raising SEGV explicitly, to ensure chaining works
  // properly in this case
  raise(SIGSEGV);
#endif

  char *bug = NULL;
  *bug = 42;

  // The crash handler should intercept the SIGSEGV, invoke the receiver,
  // and write the crash report to output_dir before the process terminates.
  return 0;
}

/* Example output file:
{
  "counters": {
    "unwinding": 0,
    "not_profiling": 0,
    "serializing": 1,
    "collecting_sample": 0
  },
  "incomplete": false,
  "metadata": {
    "library_name": "crashtracking-test",
    "library_version": "12.34.56",
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
"trace_unsynchronized<libdd_crashtracker::collectors::emit_backtrace_by_frames::{closure_env#0}<std::process::ChildStdin>>"
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
*/
