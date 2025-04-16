// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <datadog/common.h>
#include <datadog/data-pipeline.h>
#include <datadog/log.h>

enum {
    SUCCESS,
    ERROR_SEND,
};

void handle_error(ddog_TraceExporterError *err) {
    fprintf(stderr, "Operation failed with error: %d, reason: %s\n", err->code, err->msg);
    ddog_trace_exporter_error_free(err);
}

void handle_log_error(ddog_Error *err) {
    fprintf(stderr, "Operation failed with error: %d", err->message);
    ddog_Error_drop(err);
}

void log_callback(ddog_LogEvent event) {
    char* level = event.level == DDOG_LOG_EVENT_LEVEL_DEBUG ? "DEBUG" :
                  event.level == DDOG_LOG_EVENT_LEVEL_INFO ? "INFO" :
                  event.level == DDOG_LOG_EVENT_LEVEL_WARN ? "WARN" :
                  event.level == DDOG_LOG_EVENT_LEVEL_ERROR ? "ERROR" : "TRACE";

    printf("%s :: %.*s :: ", level, (int)event.message.len, (char *)event.message.ptr);
    for (size_t i = 0; i < event.fields.len; ++i) {
        printf("%.*s: %.*s%s",
               (int)event.fields.ptr[i].key.len, (char *)event.fields.ptr[i].key.ptr,
               (int)event.fields.ptr[i].value.len, (char *)event.fields.ptr[i].value.ptr,
               (i < event.fields.len - 1) ? ", " : "");
    }
    printf("\n");
}

int log_init() {
    ddog_LogEventLevel log_level = DDOG_LOG_EVENT_LEVEL_DEBUG;
    ddog_LogCallback callback = log_callback;

    // Initialize the logger
    struct ddog_Error *err = ddog_log_init(log_level, callback);
    if (err) {
        handle_log_error(err);
        return 1;
    }

    // Set the log level, just for checking the API
    err = ddog_log_set_log_level(DDOG_LOG_EVENT_LEVEL_TRACE);
    if (err) {
        handle_log_error(err);
        return 1;
    }

    return 0;
}

int main(int argc, char** argv)
{
    if (log_init() != 0) {
        fprintf(stderr, "Failed to initialize logger\n");
        return 1;
    }

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
    ddog_TraceExporterResponse *response;

    ret = ddog_trace_exporter_send(trace_exporter, buffer, 0, &response);

    assert(ret->code == DDOG_TRACE_EXPORTER_ERROR_CODE_SERDE);
    if (ret) {
        error = ERROR_SEND;
        handle_error(ret);
        goto error;
    }

    ddog_trace_exporter_response_free(response);
    ddog_trace_exporter_free(trace_exporter);
    ddog_trace_exporter_config_free(config);

    return SUCCESS;

error:
    if (trace_exporter) { ddog_trace_exporter_free(trace_exporter); }
    if (config) { ddog_trace_exporter_config_free(config); }
    return error;
}
