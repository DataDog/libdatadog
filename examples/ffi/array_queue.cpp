// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

extern "C" {
#include <datadog/common.h>
}
#include <cstdio>
#include <memory>

struct Sample {
  int x;
  int y;
};

void delete_fn(void *sample) { delete (Sample *)sample; }

void print_error(const char *s, const ddog_Error &err) {
  auto charslice = ddog_Error_message(&err);
  printf("%s (%.*s)\n", s, static_cast<int>(charslice.len), charslice.ptr);
}

int main(void) {
  auto array_queue_new_result = ddog_array_queue_new(10, delete_fn);
  if (array_queue_new_result.tag != DDOG_ARRAY_QUEUE_NEW_RESULT_OK) {
    print_error("Failed to create array queue", array_queue_new_result.err);
    ddog_Error_drop(&array_queue_new_result.err);
    return 1;
  }
  std::unique_ptr<ddog_ArrayQueue> array_queue(&array_queue_new_result.ok);
}
