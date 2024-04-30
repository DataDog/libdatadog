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
    TRY(ddog_trace_exporter_new(
        &trace_exporter,
        url,
        tracer_version,
        language,
        language_version,
        language_interpreter,
        DDOG_TRACE_EXPORTER_INPUT_FORMAT_PROXY,
        DDOG_TRACE_EXPORTER_OUTPUT_FORMAT_V04,
        &agent_response_callback
        ));

    if (trace_exporter == NULL)
    {
        printf("unable to build the trace exporter");
        return 1;
    }

    ddog_ByteSlice buffer = { .ptr = NULL, .len=0 };
    TRY(ddog_trace_exporter_send(trace_exporter, buffer, 0));

    ddog_trace_exporter_free(trace_exporter);

    return 0;
}
