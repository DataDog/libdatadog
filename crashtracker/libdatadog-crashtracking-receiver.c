// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2024-Present Datadog, Inc.

#include <datadog/common.h>
#include <datadog/profiling.h>
#include <stdio.h>
#include <stdlib.h>

int main(void) {
  ddog_prof_Profile_Result new_result = ddog_prof_crashtracker_receiver_entry_point();
  if (new_result.tag != DDOG_PROF_PROFILE_NEW_RESULT_OK) {
    ddog_CharSlice message = ddog_Error_message(&new_result.err);
    fprintf(stderr, "%*s", (int)message.len, message.ptr);
    ddog_Error_drop(&new_result.err);
    exit(EXIT_FAILURE);
  }
  return 0;
}