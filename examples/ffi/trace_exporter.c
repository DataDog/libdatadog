// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <stdio.h>
#include <stdlib.h>
#include <datadog/common.h>
#include <datadog/data-pipeline.h>

int main(int argc, char** argv)
{
    ddog_CharSlice host = DDOG_CHARSLICE_C("localhost");
    uint16_t port = 8126;
    ddog_CharSlice tracer_version = DDOG_CHARSLICE_C("v0.1");
    ddog_CharSlice language = DDOG_CHARSLICE_C("dotnet");
    ddog_CharSlice language_version = DDOG_CHARSLICE_C("10.0");
    ddog_CharSlice language_interpreter = DDOG_CHARSLICE_C("X");
    ddog_TraceExporter* trace_exporter = ddog_trace_exporter_new(
        host,
        port,
        tracer_version,
        language,
        language_version,
        language_interpreter
        );

    if (trace_exporter == NULL)
    {
        printf("unable to build the trace exporter");
        return 1;
    }

    ddog_ByteSlice buffer = { .ptr = NULL, .len=0 };
    char* str_result = ddog_trace_exporter_send(trace_exporter, buffer, 0);

    free(str_result);


    ddog_trace_exporter_free(trace_exporter);

    return 0;
}
