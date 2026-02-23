// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// FFI test for ddog_crasht_report_unhandled_exception.
//
// This test initializes the crashtracker (without a live signal handler),
// builds a small runtime StackTrace, calls report_unhandled_exception, and
// verifies that a crash report file is produced in the current directory.
//
// Usage:
//   crashtracking_unhandled_exception [receiver_binary_path]
//
// The receiver binary path may also be supplied via the
// DDOG_CRASHT_TEST_RECEIVER environment variable.  When run through
// `cargo ffi-test` the variable is set automatically.

#include <datadog/common.h>
#include <datadog/crashtracker.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static ddog_CharSlice slice(const char *s) {
  return (ddog_CharSlice){.ptr = s, .len = strlen(s)};
}

static void handle_void(ddog_VoidResult result, const char *ctx) {
  if (result.tag != DDOG_VOID_RESULT_OK) {
    ddog_CharSlice msg = ddog_Error_message(&result.err);
    fprintf(stderr, "FAIL [%s]: %.*s\n", ctx, (int)msg.len, msg.ptr);
    ddog_Error_drop(&result.err);
    exit(EXIT_FAILURE);
  }
}

static void push_named_frame(ddog_crasht_Handle_StackTrace *trace,
                              const char *function_name, uintptr_t ip) {
  ddog_crasht_StackFrame_NewResult fr = ddog_crasht_StackFrame_new();
  if (fr.tag != DDOG_CRASHT_STACK_FRAME_NEW_RESULT_OK) {
    ddog_CharSlice msg = ddog_Error_message(&fr.err);
    fprintf(stderr, "FAIL [StackFrame_new]: %.*s\n", (int)msg.len, msg.ptr);
    ddog_Error_drop(&fr.err);
    exit(EXIT_FAILURE);
  }

  ddog_crasht_Handle_StackFrame *frame =
      (ddog_crasht_Handle_StackFrame *)malloc(sizeof(*frame));
  if (!frame) {
    fputs("FAIL [malloc frame]\n", stderr);
    exit(EXIT_FAILURE);
  }
  *frame = fr.ok;

  handle_void(ddog_crasht_StackFrame_with_function(frame, slice(function_name)),
               "StackFrame_with_function");
  if (ip != 0) {
    handle_void(ddog_crasht_StackFrame_with_ip(frame, ip),
                 "StackFrame_with_ip");
  }

  /* push_frame consumes the frame */
  handle_void(ddog_crasht_StackTrace_push_frame(trace, frame, /*incomplete=*/true),
               "StackTrace_push_frame");
  free(frame);
}

// Entry point
int main(int argc, char **argv) {
  const char *receiver_path = NULL;
  if (argc >= 2) {
    receiver_path = argv[1];
  } else {
    receiver_path = getenv("DDOG_CRASHT_TEST_RECEIVER");
  }
  if (!receiver_path || receiver_path[0] == '\0') {
    fputs("FAIL: receiver binary path not provided.\n"
          "      Pass it as argv[1] or set DDOG_CRASHT_TEST_RECEIVER.\n",
          stderr);
    return EXIT_FAILURE;
  }

  static const char output_file[] = "crashreport_unhandled_exception.json";
  static const char stderr_file[] = "crashreport_unhandled_exception.stderr";
  static const char stdout_file[] = "crashreport_unhandled_exception.stdout";

  // Forward the dynamic-linker search path to the receiver process.
  // The receiver is execve'd with an explicit environment so it does not
  // inherit the parent's env automatically.  The variable name differs by OS:
  //   Linux / ELF  → LD_LIBRARY_PATH
  //   macOS        → DYLD_LIBRARY_PATH
#ifdef __APPLE__
  const char *ld_search_path_var   = "DYLD_LIBRARY_PATH";
#else
  const char *ld_search_path_var   = "LD_LIBRARY_PATH";
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
      .path_to_receiver_binary = slice(receiver_path),
      .optional_stderr_filename = slice(stderr_file),
      .optional_stdout_filename = slice(stdout_file),
      .env = env_slice,
  };

  struct ddog_Endpoint *endpoint =
      ddog_endpoint_from_filename(slice(output_file));

  struct ddog_crasht_Slice_CInt signals = ddog_crasht_default_signals();
  ddog_crasht_Config config = {
      .create_alt_stack = false,
      .endpoint = endpoint,
      .resolve_frames = DDOG_CRASHT_STACKTRACE_COLLECTION_DISABLED,
      .signals = {.ptr = signals.ptr, .len = signals.len},
  };

  ddog_crasht_Metadata metadata = {
      .library_name    = slice("crashtracking-ffi-test"),
      .library_version = slice("0.0.0"),
      .family          = slice("native"),
      .tags            = NULL,
  };

  handle_void(ddog_crasht_init(config, receiver_config, metadata),
               "ddog_crasht_init");
  ddog_endpoint_drop(endpoint);

  // Build a runtime StackTrace with two synthetic frames.
  ddog_crasht_StackTrace_NewResult tr = ddog_crasht_StackTrace_new();
  if (tr.tag != DDOG_CRASHT_STACK_TRACE_NEW_RESULT_OK) {
    ddog_CharSlice msg = ddog_Error_message(&tr.err);
    fprintf(stderr, "FAIL [StackTrace_new]: %.*s\n", (int)msg.len, msg.ptr);
    ddog_Error_drop(&tr.err);
    return EXIT_FAILURE;
  }

  ddog_crasht_Handle_StackTrace *trace =
      (ddog_crasht_Handle_StackTrace *)malloc(sizeof(*trace));
  if (!trace) {
    fputs("FAIL [malloc trace]\n", stderr);
    return EXIT_FAILURE;
  }
  *trace = tr.ok;

  push_named_frame(trace, "com.example.MyApp.processRequest", 0x1000);
  push_named_frame(trace, "com.example.runtime.EventLoop.run",  0x2000);
  push_named_frame(trace, "com.example.runtime.main",           0x3000);

  handle_void(ddog_crasht_StackTrace_set_complete(trace),
               "StackTrace_set_complete");

  // Report the unhandled exception.  This call:
  //   - spawns the receiver process,
  //   - sends the crash report over the socket,
  //   - waits for the receiver to finish writing the report,
  //   - returns Ok on success.
  handle_void(
      ddog_crasht_report_unhandled_exception(
          slice("com.example.UncaughtRuntimeException"),
          slice("Something went very wrong in the runtime"),
          trace),
      "ddog_crasht_report_unhandled_exception");

  free(trace);

  // Verify a report file was produced.
  FILE *f = fopen(output_file, "r");
  if (!f) {
    fprintf(stderr, "FAIL: expected crash report at '%s' but file not found\n",
            output_file);
    return EXIT_FAILURE;
  }
  fclose(f);

  printf("PASS: crash report written to '%s'\n", output_file);
  return EXIT_SUCCESS;
}

