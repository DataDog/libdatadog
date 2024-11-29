// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <stdio.h>
#include <stdlib.h>
#include <datadog/common.h>
#include <datadog/data-pipeline.h>

#define TRY(expr)                                                                                  \
  {                                                                                                \
    ddog_MaybeError err = expr;                                                                    \
    if (err.tag == DDOG_OPTION_ERROR_SOME_ERROR) {                                                 \
      ddog_CharSlice message = ddog_Error_message(&err.some);                                      \
      fprintf(stderr, "ERROR: %.*s", (int)message.len, (char *)message.ptr);                       \
      return 1;                                                                                    \
    }                                                                                              \
  }

void agent_response_callback(const char* response)
{
    printf("Agent response: %s\n", response);
}

int main(int argc, char** argv)
{
    ddog_TraceExporter* trace_exporter;
    ddog_CharSlice url = DDOG_CHARSLICE_C("http://localhost:8126/");
    ddog_CharSlice tracer_version = DDOG_CHARSLICE_C("v0.1");
    ddog_CharSlice language = DDOG_CHARSLICE_C("dotnet");
    ddog_CharSlice language_version = DDOG_CHARSLICE_C("10.0");
    ddog_CharSlice language_interpreter = DDOG_CHARSLICE_C("X");
    ddog_CharSlice hostname = DDOG_CHARSLICE_C("host1");
    ddog_CharSlice env = DDOG_CHARSLICE_C("staging");
    ddog_CharSlice version = DDOG_CHARSLICE_C("1.0");
    ddog_CharSlice service = DDOG_CHARSLICE_C("test_app");


    ddog_TraceExporterConfig *config;
    ddog_trace_exporter_config_new(&config);
    ddog_trace_exporter_config_set_url(config, url);
    ddog_trace_exporter_config_set_tracer_version(config, tracer_version);
    ddog_trace_exporter_config_set_language(config, language);

    TRY(ddog_trace_exporter_new(&trace_exporter, config));

    if (trace_exporter == NULL)
    {
        printf("unable to build the trace exporter");
        return 1;
    }

    ddog_ByteSlice buffer = { .ptr = NULL, .len=0 };
    ddog_AgentResponse response;

    TRY(ddog_trace_exporter_send(trace_exporter, buffer, 0, &response));

    ddog_trace_exporter_free(trace_exporter);

    return 0;
}
