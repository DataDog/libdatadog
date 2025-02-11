// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/common.h>
#include <datadog/crashtracker.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>

#define CRASH_REPORTS_DIR "/tmp/crashreports"
#define TIMESTAMP_FORMAT "%Y%m%d_%H%M%S"

void example_segfault_handler(int signal) {
  fprintf(stderr, "Segmentation fault caught. Signal number: %d\n", signal);
  exit(EXIT_FAILURE);
}

void handle_result(ddog_VoidResult result) {
  if (result.tag == DDOG_VOID_RESULT_ERR) {
    ddog_CharSlice message = ddog_Error_message(&result.err);
    fprintf(stderr, "Error: %.*s\n", (int)message.len, message.ptr);
    ddog_Error_drop(&result.err);
    exit(EXIT_FAILURE);
  }
}

uintptr_t handle_uintptr_t_result(ddog_crasht_Result_Usize result) {
  if (result.tag == DDOG_CRASHT_RESULT_USIZE_ERR_USIZE) {
    ddog_CharSlice message = ddog_Error_message(&result.err);
    fprintf(stderr, "Error: %.*s\n", (int)message.len, message.ptr);
    ddog_Error_drop(&result.err);
    exit(EXIT_FAILURE);
  }
  return result.ok;
}

void ensure_directory_exists(const char *path) {
  struct stat st = {0};
  if (stat(path, &st) == -1) {
    if (mkdir(path, 0700) == -1) {
      fprintf(stderr, "Failed to create directory: %s\n", path);
      exit(EXIT_FAILURE);
    }
  }
}

void generate_unique_filename(char *buffer, size_t len) {
  time_t now = time(NULL);
  struct tm t;
  if (localtime_r(&now, &t) == NULL) {
    perror("Failed to get local time");
    exit(EXIT_FAILURE);
  }

  // Format timestamp and append to directory path
  snprintf(buffer, len, "%s/crashreport_", CRASH_REPORTS_DIR);
  strftime(buffer + strlen(buffer), len - strlen(buffer), TIMESTAMP_FORMAT, &t);
  strncat(buffer, ".json", len - strlen(buffer) - 1);
}

int main(int argc, char **argv) {
  if (signal(SIGSEGV, example_segfault_handler) == SIG_ERR) {
    perror("Error setting up signal handler");
    return EXIT_FAILURE;
  }

  // Ensure the directory for crash reports exists
  ensure_directory_exists(CRASH_REPORTS_DIR);

  // Generate a unique crash report filename
  char crash_report_filename[256];
  generate_unique_filename(crash_report_filename, sizeof(crash_report_filename));
  fprintf(stderr, "Using crash report file: %s\n", crash_report_filename);

  // Set the receiver binary path using DATADOG_ROOT
  const char *receiver_binary = DATADOG_ROOT "/bin/libdatadog-crashtracking-receiver";
  fprintf(stderr, "Using receiver binary: %s\n", receiver_binary);

  ddog_crasht_ReceiverConfig receiver_config = {
      .args = {},
      .env = {},
      .path_to_receiver_binary = {.ptr = receiver_binary, .len = strlen(receiver_binary)},
      .optional_stderr_filename = DDOG_CHARSLICE_C(CRASH_REPORTS_DIR "/stderr.txt"),
      .optional_stdout_filename = DDOG_CHARSLICE_C(CRASH_REPORTS_DIR "/stdout.txt"),
  };

  ddog_CharSlice report_filename_slice = {.ptr = crash_report_filename, .len = strlen(crash_report_filename)};
  struct ddog_Endpoint *endpoint = ddog_endpoint_from_filename(report_filename_slice);

  ddog_crasht_Config config = {
      .create_alt_stack = true,
      .use_alt_stack = true,
      .endpoint = endpoint,
      .resolve_frames = DDOG_CRASHT_STACKTRACE_COLLECTION_ENABLED_WITH_INPROCESS_SYMBOLS,
  };

  ddog_crasht_Metadata metadata = {
      .library_name = DDOG_CHARSLICE_C("crashtracking-test"),
      .library_version = DDOG_CHARSLICE_C("12.34.56"),
      .family = DDOG_CHARSLICE_C("crashtracking-test"),
      .tags = NULL,
  };

  handle_result(ddog_crasht_init(config, receiver_config, metadata));
  ddog_endpoint_drop(endpoint);

  handle_result(ddog_crasht_begin_op(DDOG_CRASHT_OP_TYPES_PROFILER_COLLECTING_SAMPLE));
  handle_uintptr_t_result(ddog_crasht_insert_span_id(0, 42));
  handle_uintptr_t_result(ddog_crasht_insert_trace_id(1, 1));
  handle_uintptr_t_result(ddog_crasht_insert_additional_tag(
      DDOG_CHARSLICE_C("This is a very informative extra bit of info")));
  handle_uintptr_t_result(ddog_crasht_insert_additional_tag(
      DDOG_CHARSLICE_C("This is a very informative extra bit of info")));

#ifdef EXPLICIT_RAISE_SEGV
  raise(SIGSEGV); // Explicitly trigger segmentation fault for testing
#endif

  char *bug = NULL;
  *bug = 42; // Cause segmentation fault
  // find the expected crashtracking files in:
  // ls /tmp/crashreports
  // crashreport_20240719_165216.json stderr.txt stdout.txt
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
*/
