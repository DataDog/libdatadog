// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/common.h>
#include <datadog/profiling.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>

void example_segfault_handler(int signal) {
  printf("Segmentation fault caught. Signal number: %d\n", signal);
  exit(-1);
}

int main(int argc, char **argv) {
  if (signal(SIGSEGV, example_segfault_handler) == SIG_ERR) {
    perror("Error setting up signal handler");
    return -1;
  }

  ddog_prof_CrashtrackerReceiverConfig receiver_config = {
      .args = {},
      .env = {},
      //.path_to_receiver_binary = DDOG_CHARSLICE_C("SET ME TO THE ACTUAL PATH ON YOUR MACHINE"),
      // E.g. on my machine, where I run ./build-profiling-ffi.sh build-ffi
      .path_to_receiver_binary =
          DDOG_CHARSLICE_C("/Users/daniel.schwartznarbonne/go/src/github.com/DataDog/libdatadog/"
                           "build-ffi/bin/libdatadog-crashtracking-receiver"),
      .optional_stderr_filename = DDOG_CHARSLICE_C("/tmp/crashreports/stderr.txt"),
      .optional_stdout_filename = DDOG_CHARSLICE_C("/tmp/crashreports/stdout.txt"),
  };

  ddog_prof_CrashtrackerConfiguration config = {
      .create_alt_stack = false,
      .endpoint = ddog_Endpoint_file(DDOG_CHARSLICE_C("/tmp/crashreports/foo.txt")),
      // Alternatively:
      //.endpoint = ddog_prof_Endpoint_agent(DDOG_CHARSLICE_C("http://localhost:8126")),
      .resolve_frames = DDOG_PROF_STACKTRACE_COLLECTION_ENABLED_WITH_INPROCESS_SYMBOLS,
  };

  ddog_prof_CrashtrackerMetadata metadata = {
      .profiling_library_name = DDOG_CHARSLICE_C("crashtracking-test"),
      .profiling_library_version = DDOG_CHARSLICE_C("12.34.56"),
      .family = DDOG_CHARSLICE_C("crashtracking-test"),
      .tags = NULL,
  };

  ddog_prof_CrashtrackerResult result =
      ddog_prof_Crashtracker_init_with_receiver(config, receiver_config, metadata);
  if (result.tag == DDOG_PROF_PROFILE_RESULT_ERR) {
    ddog_CharSlice message = ddog_Error_message(&result.err);
    fprintf(stderr, "%.*s\n", (int)message.len, message.ptr);
    ddog_Error_drop(&result.err);
    return -1;
  }

#ifdef EXPLICIT_RAISE_SEGV
  // Test raising SEGV explicitly, to ensure chaining works
  // properly in this case
  raise(SIGSEGV);
#endif

  char *bug = NULL;
  *bug = 42;

  // At this point, we expect the following files to be written into /tmp/crashreports
  // foo.txt  foo.txt.telemetry  stderr.txt  stdout.txt
  return 0;
}
