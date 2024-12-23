// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/common.h>
#include <datadog/telemetry.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifndef _WIN32
#include <unistd.h>
#else
#include <windows.h>
#endif

#define TRY(expr)                                                                                  \
  {                                                                                                \
    ddog_MaybeError err = expr;                                                                    \
    if (err.tag == DDOG_OPTION_ERROR_SOME_ERROR) {                                                 \
      ddog_CharSlice message = ddog_Error_message(&err.some);                                      \
      fprintf(stderr, "ERROR: %.*s", (int)message.len, (char *)message.ptr);                       \
      return 1;                                                                                    \
    }                                                                                              \
  }

#define STR(x) #x
#define LOG_LOCATION_IDENTIFIER() DDOG_CHARSLICE_C(STR(__FILE__) ":" STR(__LINE__))

#ifdef _WIN32
unsigned int sleep(unsigned int seconds) {
    Sleep(seconds * 1000);
    return 0;
}
#endif


ddog_CharSlice charslice_from_ptr(char *str) {
  return (ddog_CharSlice){
      .ptr = str,
      .len = strlen(str),
  };
}

int main(void) {
  ddog_TelemetryWorkerBuilder *builder;
  ddog_CharSlice service = DDOG_CHARSLICE_C("rust"), lang = DDOG_CHARSLICE_C("libdatadog-example"),
                 lang_version = DDOG_CHARSLICE_C("1.69.0"),
                 tracer_version = DDOG_CHARSLICE_C("0.0.0");
  TRY(ddog_telemetry_builder_instantiate(&builder, service, lang, lang_version, tracer_version));

  ddog_CharSlice endpoint_char = DDOG_CHARSLICE_C("file://./examples_telemetry_metrics.out");
  struct ddog_Endpoint *endpoint = ddog_endpoint_from_url(endpoint_char);
  TRY(ddog_telemetry_builder_with_endpoint_config_endpoint(builder, endpoint));
  ddog_endpoint_drop(endpoint);

  ddog_CharSlice runtime_id = DDOG_CHARSLICE_C("fa1f0ed0-8a3a-49e8-8f23-46fb44e24579"),
                 service_version = DDOG_CHARSLICE_C("1.0"), env = DDOG_CHARSLICE_C("test");
  TRY(ddog_telemetry_builder_with_str_runtime_id(builder, runtime_id));
  TRY(ddog_telemetry_builder_with_str_application_service_version(builder, service_version));
  TRY(ddog_telemetry_builder_with_str_application_env(builder, env));

  TRY(ddog_telemetry_builder_with_bool_config_telemetry_debug_logging_enabled(builder, true));

  ddog_TelemetryWorkerHandle *handle;
  // builder is consummed after the call to build
  TRY(ddog_telemetry_builder_run_metric_logs(builder, &handle));
  TRY(ddog_telemetry_handle_start(handle));

  ddog_CharSlice metric_name = DDOG_CHARSLICE_C("test.telemetry");
  ddog_Vec_Tag tags = ddog_Vec_Tag_new();
  ddog_Vec_Tag_push(&tags, charslice_from_ptr("foo"), charslice_from_ptr("bar"));
  // tags is consummed
  struct ddog_ContextKey test_temetry = ddog_telemetry_handle_register_metric_context(
      handle, metric_name, DDOG_METRIC_TYPE_COUNT, tags, true, DDOG_METRIC_NAMESPACE_TELEMETRY);

  TRY(ddog_telemetry_handle_add_point(handle, &test_temetry, 1.0));
  TRY(ddog_telemetry_handle_add_point(handle, &test_temetry, 1.0));

  ddog_Vec_Tag extra_tags = ddog_Vec_Tag_new();
  ddog_Vec_Tag_push(&tags, charslice_from_ptr("baz"), charslice_from_ptr("bat"));
  TRY(ddog_telemetry_handle_add_point_with_tags(handle, &test_temetry, 1.0, extra_tags));
  for (int i = 0; i < 10; i++) {
    TRY(ddog_telemetry_handle_add_log(
        handle, LOG_LOCATION_IDENTIFIER(),
        DDOG_CHARSLICE_C("no kinder bueno left in the cafetaria"),
        DDOG_LOG_LEVEL_ERROR, DDOG_CHARSLICE_C("")));
  }

  sleep(11);
  TRY(ddog_telemetry_handle_stop(handle));
  ddog_telemetry_handle_wait_for_shutdown_ms(handle, 10);

  return 0;
}
