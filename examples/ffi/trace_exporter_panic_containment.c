// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <stdint.h>
#include <stdio.h>
#include <datadog/data-pipeline.h>

int main(void)
{
    ddog_TracerTraceChunks *chunks = NULL;
    ddog_TraceExporterError *err = ddog_tracer_trace_chunks_new(SIZE_MAX, &chunks);

    if (err == NULL) {
        fprintf(stderr, "capacity overflow unexpectedly succeeded\n");
        if (chunks != NULL) {
            ddog_tracer_trace_chunks_free(chunks);
        }
        return 1;
    }

    int status = 0;
    if (err->code != DDOG_TRACE_EXPORTER_ERROR_CODE_PANIC) {
        fprintf(stderr, "capacity overflow returned error code %d instead of panic\n", err->code);
        status = 1;
    }

    ddog_trace_exporter_error_free(err);
    return status;
}
