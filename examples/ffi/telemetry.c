// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/common.h>
#include <datadog/telemetry.h>
#include <stdio.h>
#include <stdlib.h>

#define TRY(expr)                                                                                  \
  {                                                                                                \
    ddog_MaybeError err = expr;                                                                    \
    if (err.tag == DDOG_OPTION_VEC_U8_SOME_VEC_U8) {                                               \
      fprintf("ERROR: %.*s", err.some.ptr, err.some.len);                                          \
      return 1;                                                                                    \
    }                                                                                              \
  }

int main(void) {
  ddog_TelemetryWorkerBuilder *builder;
  ddog_CharSlice service = DDOG_CHARSLICE_C("rust"), lang = DDOG_CHARSLICE_C("libdatadog-example"),
                 lang_version = DDOG_CHARSLICE_C("1.69.0"),
                 tracer_version = DDOG_CHARSLICE_C("0.0.0");
  TRY(ddog_builder_instantiate(&builder, service, lang, lang_version, tracer_version));

  ddog_CharSlice endpoint_char = DDOG_CHARSLICE_C("file://./examples_telemetry.out");
  struct ddog_Endpoint *endpoint = ddog_endpoint_from_url(endpoint_char);
  TRY(ddog_builder_with_endpoint_config_endpoint(builder, endpoint));
  ddog_endpoint_drop(endpoint);

  ddog_CharSlice runtime_id = DDOG_CHARSLICE_C("fa1f0ed0-8a3a-49e8-8f23-46fb44e24579"),
                 service_version = DDOG_CHARSLICE_C("1.0"), env = DDOG_CHARSLICE_C("test");
  TRY(ddog_builder_with_str_runtime_id(builder, runtime_id));
  TRY(ddog_builder_with_str_application_service_version(builder, service_version));
  TRY(ddog_builder_with_str_application_env(builder, env));

  TRY(ddog_builder_with_bool_config_telemetry_debug_logging_enabled(builder, true));

  ddog_TelemetryWorkerHandle *handle;
  TRY(ddog_builder_run(builder, &handle));
  TRY(ddog_handle_start(handle));

  TRY(ddog_handle_stop(handle));
  ddog_handle_wait_for_shutdown(handle);

  return 0;
}