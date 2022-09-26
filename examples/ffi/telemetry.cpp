// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present
// Datadog, Inc.

extern "C" {
#include <datadog/common.h>
#include <datadog/telemetry.h>
}
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <thread>


#define TRY(operation, error_message) do { \
  err = (operation) ; \
  if(err.tag == DDOG_OPTION_VEC_U8_SOME_VEC_U8) { \
    print_error(error_message, &err.some); \
    return 1; \
  } \
} while (false)

void print_error(const char *s, const ddog_Vec_u8 *err) {
  printf("%s (%.*s)\n", s, (int)(err->len), err->ptr);
}

int main(void) {
  ddog_MaybeError err = {
    .tag = DDOG_OPTION_VEC_U8_NONE_VEC_U8,
  };
  ddog_TelemetryWorkerBuilder* builder = NULL;

  ddog_builder_instantiate(
    &builder,
    DDOG_CHARSLICE_C("ffi-test"),
    DDOG_CHARSLICE_C("cpp"),
    DDOG_CHARSLICE_C("C89"),
    DDOG_CHARSLICE_C("0.8")
  );

  // Set properties by name
  ddog_builder_with_runtime_id(builder, DDOG_CHARSLICE_C("58a260d7-4309-45bc-a167-7a64abea3da6"));
  // Set property from enum value
  ddog_builder_with_property(builder, DDOG_TELEMETRY_WORKER_BUILDER_PROPERTY_APPLICATION_SERVICE_VERSION,DDOG_CHARSLICE_C("0.0.1"));
  // Set properties by string path
  TRY(
    ddog_builder_with_str_property(builder, DDOG_CHARSLICE_C("application.env"), DDOG_CHARSLICE_C("test")),
    "Setting key application.env on builder"
  );

  ddog_TelemetryWorkerHandle *handle = NULL;

  TRY(
    ddog_builder_run(builder, &handle),
    "Running the worker"
  );

  TRY(
    ddog_handle_add_dependency(handle, DDOG_CHARSLICE_C("libdatadog"), DDOG_CHARSLICE_C("0.8")),
    "Adding dependency"
  );
  TRY(
    ddog_handle_start(handle),
    "Starting the worker"
  );

  ddog_ContextKey libbdatadog_test_key = ddog_handle_register_metric_context(
    handle,
    DDOG_CHARSLICE_C("libdatadog.test"),
    ddog_Vec_tag_new(),
    DDOG_METRIC_TYPE_COUNT,
    false,
    DDOG_METRIC_NAMESPACE_TRACE
  );

  ddog_handle_add_point(handle, &libbdatadog_test_key, 1.0, ddog_Vec_tag_new());
  ddog_handle_add_point(handle, &libbdatadog_test_key, 2.0, ddog_Vec_tag_new());

  ddog_ParseTagsResult parsed_tags = ddog_Vec_tag_parse(DDOG_CHARSLICE_C("foo:a,bar:b"));
  if (parsed_tags.error_message != NULL) {
    print_error("Parsing tags", parsed_tags.error_message);
  }
  ddog_handle_add_point(handle, &libbdatadog_test_key, 7.0, parsed_tags.tags);

  std::this_thread::sleep_for(std::chrono::seconds(20));

  TRY(
    ddog_handle_stop(handle),
    "Sending stop message to the worker"
  );
  ddog_handle_wait_for_shutdown(handle);
  return 0;
}
