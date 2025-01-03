// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/common.h>
#include <datadog/crashtracker.h>
#include <stdio.h>
#include <stdlib.h>

int main(void) {
  ddog_VoidResult new_result = ddog_crasht_receiver_entry_point_stdin();
  if (new_result.tag != DDOG_VOID_RESULT_OK) {
    ddog_CharSlice message = ddog_Error_message(&new_result.err);
    fprintf(stderr, "%.*s", (int)message.len, message.ptr);
    ddog_Error_drop(&new_result.err);
    exit(EXIT_FAILURE);
  }
  return 0;
}
