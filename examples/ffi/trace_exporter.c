// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
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
    fprintf(stderr, "Operation failed with error: %s\n", (char *)err->message.ptr);
    ddog_Error_drop(err);
}

int log_init(const char* log_path) {
    // Always configure console logging to stdout
    struct ddog_StdConfig std_config = {
        .target = DDOG_STD_TARGET_OUT
    };
    struct ddog_Error *err = ddog_logger_configure_std(std_config);
    if (err) {
        handle_log_error(err);
        return 1;
    }

    // Additionally configure file logging if path is provided
    if (log_path != NULL) {
        struct ddog_FileConfig file_config = {
            .path = (ddog_CharSlice){
                .ptr = log_path,
                .len = strlen(log_path)
            },
            .max_size_bytes = 0,
            .max_files = 0
        };
        err = ddog_logger_configure_file(file_config);
        if (err) {
            handle_log_error(err);
            return 1;
        }
    }

    // Set the log level to TRACE for maximum verbosity
    err = ddog_logger_set_log_level(DDOG_LOG_EVENT_LEVEL_TRACE);
    if (err) {
        handle_log_error(err);
        return 1;
    }

    return 0;
}

int main(int argc, char** argv)
{
    // Initialize logger with optional path from command line
    const char* log_path = (argc > 1) ? argv[1] : NULL;
    if (log_init(log_path) != 0) {
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

    ddog_TelemetryClientConfig telemetry_config = {
        .interval = 60000,
        .runtime_id = DDOG_CHARSLICE_C("12345678-1234-1234-1234-123456789abc"),
        .debug_enabled = true
    };

    ret = ddog_trace_exporter_config_enable_telemetry(config, &telemetry_config);
    if (ret) {
        handle_error(ret);
        goto error;
    }

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

    // Disable file logging if it was enabled
    if (log_path != NULL) {
        struct ddog_Error *err = ddog_logger_disable_file();
        if (err) {
            handle_log_error(err);
        }
    }

    // disable std logging as well
    struct ddog_Error *err = ddog_logger_disable_std();
    if (err) {
        handle_log_error(err);
    }

    return SUCCESS;

error:
    if (trace_exporter) { ddog_trace_exporter_free(trace_exporter); }
    if (config) { ddog_trace_exporter_config_free(config); }
    return error;
}
