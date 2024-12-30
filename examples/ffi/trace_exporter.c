// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <datadog/common.h>
#include <datadog/data-pipeline.h>

enum {
    SUCCESS,
    ERROR_SEND,
};

void handle_error(ddog_TraceExporterError *err) {
    fprintf(stderr, "Operation failed with error: %d, reason: %s\n", err->code, err->msg);
    ddog_trace_exporter_error_free(err);
}

int main(int argc, char** argv)
{
    int error;

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


    ddog_TraceExporterError *ret;
    ddog_TraceExporterConfig *config;

    ddog_trace_exporter_config_new(&config);
    ddog_trace_exporter_config_set_url(config, url);
    ddog_trace_exporter_config_set_tracer_version(config, tracer_version);
    ddog_trace_exporter_config_set_language(config, language);


    ret = ddog_trace_exporter_new(&trace_exporter, config);

    assert(ret == NULL);
    assert(trace_exporter != NULL);

    ddog_ByteSlice buffer = { .ptr = NULL, .len=0 };
    ddog_AgentResponse response;

    ret = ddog_trace_exporter_send(trace_exporter, buffer, 0, &response);

    assert(ret->code == DDOG_TRACE_EXPORTER_ERROR_CODE_SERDE);
    if (ret) {
        error = ERROR_SEND;
        handle_error(ret);
        goto error;
    }

    ddog_trace_exporter_free(trace_exporter);
    ddog_trace_exporter_config_free(config);

    return SUCCESS;

error:
    if (trace_exporter) { ddog_trace_exporter_free(trace_exporter); }
    if (config) { ddog_trace_exporter_config_free(config); }
    return error;
}
