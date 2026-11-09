// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/common.h>
#include <datadog/shared-runtime.h>
#include <datadog/telemetry.h>
#include <stdio.h>

#define TRY(expr)                                                                                  \
  {                                                                                                \
    ddog_MaybeError err = expr;                                                                    \
    if (err.tag == DDOG_OPTION_ERROR_SOME_ERROR) {                                                 \
      ddog_CharSlice message = ddog_Error_message(&err.some);                                      \
      fprintf(stderr, "ERROR: %.*s", (int)message.len, (char *)message.ptr);                       \
      return 1;                                                                                    \
    }                                                                                              \
  }

int main(void) {
  ddog_TelemetryWorkerBuilder *builder;
  ddog_CharSlice service = DDOG_CHARSLICE_C("rust"), lang = DDOG_CHARSLICE_C("libdatadog-example"),
                 lang_version = DDOG_CHARSLICE_C("1.69.0"),
                 tracer_version = DDOG_CHARSLICE_C("0.0.0");
  TRY(ddog_telemetry_builder_instantiate(&builder, service, lang, lang_version, tracer_version));

  ddog_CharSlice endpoint_char = DDOG_CHARSLICE_C("file://./examples_telemetry.out");
  TRY(ddog_telemetry_builder_with_endpoint_config_endpoint(
      builder, endpoint_char, DDOG_CHARSLICE_C(""), 0, DDOG_CHARSLICE_C(""), false));

  ddog_CharSlice runtime_id = DDOG_CHARSLICE_C("fa1f0ed0-8a3a-49e8-8f23-46fb44e24579"),
                 service_version = DDOG_CHARSLICE_C("1.0"), env = DDOG_CHARSLICE_C("test");
  TRY(ddog_telemetry_builder_with_str_runtime_id(builder, runtime_id));
  TRY(ddog_telemetry_builder_with_str_application_service_version(builder, service_version));
  TRY(ddog_telemetry_builder_with_str_application_env(builder, env));

  TRY(ddog_telemetry_builder_with_bool_config_telemetry_debug_logging_enabled(builder, true));
  TRY(ddog_telemetry_builder_with_shutdown_timeout_ms(builder, 1000));

  ddog_TelemetryWorkerHandle *handle;
  const ddog_ForkSafeRuntime *runtime;
  ddog_SharedRuntimeFFIError *runtime_err = ddog_shared_runtime_new(&runtime);
  if (runtime_err) {
    fprintf(stderr, "ERROR: could not create the shared runtime\n");
    ddog_shared_runtime_error_free(runtime_err);
    return 1;
  }
  ddog_TelemetryWorkerRuntimeHandle *worker;
  TRY(ddog_telemetry_builder_spawn(builder, runtime, false, &handle, &worker));
  TRY(ddog_telemetry_handle_start(handle));

  TRY(ddog_telemetry_handle_stop(handle));
  // Blocks until the final flush is done, at most the shutdown timeout.
  TRY(ddog_telemetry_worker_stop(worker));
  ddog_telemetry_handle_drop(handle);
  ddog_shared_runtime_free(runtime);

  return 0;
}
