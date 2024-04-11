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
      .path_to_receiver_binary = DDOG_CHARSLICE_C("FIXME - point me to receiver binary path"),
      .optional_stderr_filename = {},
      .optional_stdout_filename = {},
  };

  ddog_prof_CrashtrackerConfiguration config = {
      .create_alt_stack = false,
      .endpoint = ddog_prof_Endpoint_agent(DDOG_CHARSLICE_C("http://localhost:8126")),
      .resolve_frames = DDOG_PROF_CRASHTRACKER_RESOLVE_FRAMES_NEVER,
  };

  ddog_prof_CrashtrackerMetadata metadata = {
      .profiling_library_name = DDOG_CHARSLICE_C("crashtracking-test"),
      .profiling_library_version = DDOG_CHARSLICE_C("12.34.56"),
      .family = DDOG_CHARSLICE_C("crashtracking-test"),
      .tags = NULL,
  };

  ddog_prof_CrashtrackerResult result =
      ddog_prof_Crashtracker_init(config, receiver_config, metadata);
  if (result.tag == DDOG_PROF_PROFILE_RESULT_ERR) {
    ddog_CharSlice message = ddog_Error_message(&result.err);
    fprintf(stderr, "%*s\n", (int)message.len, message.ptr);
    ddog_Error_drop(&result.err);
    return -1;
  }

  // raise(SIGSEGV);
  char *bug = NULL;
  *bug = 42;

  return 0;
}
