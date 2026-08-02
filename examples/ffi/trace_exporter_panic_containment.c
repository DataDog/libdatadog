// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// Proves a Rust panic is contained inside the combined profiling artifact and
// surfaced to C as an error rather than aborting the process.

#include <stdint.h>
#include <stdio.h>
#include <datadog/data-pipeline.h>

int main(void)
{
    ddog_TracerTraceChunks *chunks = NULL;

    // Positive control: without it, a build that failed every call would still
    // satisfy the panic assertion below.
    ddog_TraceExporterError *err = ddog_tracer_trace_chunks_new(0, &chunks);
    if (err != NULL) {
        fprintf(stderr, "trace_chunks_new(0) failed with error code %d\n", err->code);
        ddog_trace_exporter_error_free(err);
        return 1;
    }
    if (chunks == NULL) {
        fprintf(stderr, "trace_chunks_new(0) returned success with a null handle\n");
        return 1;
    }
    ddog_tracer_trace_chunks_free(chunks);
    // The panic path never writes out_handle, so reset before reusing it.
    chunks = NULL;

    err = ddog_tracer_trace_chunks_new(SIZE_MAX, &chunks);

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
