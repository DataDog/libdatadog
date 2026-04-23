// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <datadog/common.h>
#include <datadog/data-pipeline.h>
#include <datadog/log.h>

#define SUCCESS 0

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

    ddog_TraceExporter* trace_exporter = NULL;
    ddog_CharSlice url = DDOG_CHARSLICE_C("http://localhost:8126/");
    ddog_CharSlice tracer_version = DDOG_CHARSLICE_C("v0.1");
    ddog_CharSlice language = DDOG_CHARSLICE_C("dotnet");
    ddog_CharSlice language_version = DDOG_CHARSLICE_C("10.0");
    ddog_CharSlice language_interpreter = DDOG_CHARSLICE_C("X");
    ddog_CharSlice hostname = DDOG_CHARSLICE_C("host1");
    ddog_CharSlice env = DDOG_CHARSLICE_C("staging");
    ddog_CharSlice version = DDOG_CHARSLICE_C("1.0");
    ddog_CharSlice service = DDOG_CHARSLICE_C("test_app");

    ddog_TraceExporterError *ret = NULL;
    ddog_TraceExporterConfig *config = NULL;

    ddog_trace_exporter_config_new(&config);
    ddog_trace_exporter_config_set_url(config, url);
    ddog_trace_exporter_config_set_tracer_version(config, tracer_version);
    ddog_trace_exporter_config_set_language(config, language);
    ddog_trace_exporter_config_set_lang_version(config, language_version);
    ddog_trace_exporter_config_set_lang_interpreter(config, language_interpreter);
    ddog_trace_exporter_config_set_hostname(config, hostname);
    ddog_trace_exporter_config_set_env(config, env);
    ddog_trace_exporter_config_set_version(config, version);
    ddog_trace_exporter_config_set_service(config, service);
    ddog_trace_exporter_config_set_connection_timeout(config, 1000);

    ddog_TelemetryClientConfig telemetry_config = {
        .interval = 60000,
        .runtime_id = DDOG_CHARSLICE_C("12345678-1234-1234-1234-123456789abc"),
        .debug_enabled = true,
        .session_id = DDOG_CHARSLICE_C("12345678-1234-1234-1234-123456789abc"),
        .root_session_id = DDOG_CHARSLICE_C("87654321-1234-1234-1234-123456789abc"),
        .parent_session_id = DDOG_CHARSLICE_C(""),
    };

    ret = ddog_trace_exporter_config_enable_telemetry(config, &telemetry_config);
    if (ret) {
        error = ret->code;
        handle_error(ret);
        goto error;
    }

    ret = ddog_trace_exporter_new(&trace_exporter, config);
    if (ret) {
        error = ret->code;
        handle_error(ret);
        goto error;
    }

    printf("TraceExporter created successfully\n");

    // Construct a minimal valid msgpack V04 trace payload: [[{span}]]
    // One trace containing one span with the 7 required fields:
    // service, name, resource, trace_id, span_id, start, duration
    // Hand rolling a payload like this is not scalable. If we need to
    // do this more than once, please write a helper function.
    static const uint8_t trace_payload[] = {
        0x91,                                                       // array(1): one trace
        0x91,                                                       // array(1): one span
        0x87,                                                       // map(7): span fields
        0xa7, 's','e','r','v','i','c','e',                          // key: "service"
        0xa8, 't','e','s','t','_','a','p','p',                      // val: "test_app"
        0xa4, 'n','a','m','e',                                      // key: "name"
        0xab, 'w','e','b','.','r','e','q','u','e','s','t',          // val: "web.request"
        0xa8, 'r','e','s','o','u','r','c','e',                      // key: "resource"
        0xaa, 'G','E','T',' ','/','h','e','l','l','o',              // val: "GET /hello"
        0xa8, 't','r','a','c','e','_','i','d',                      // key: "trace_id"
        0x01,                                                       // val: 1
        0xa7, 's','p','a','n','_','i','d',                          // key: "span_id"
        0x01,                                                       // val: 1
        0xa5, 's','t','a','r','t',                                  // key: "start"
        0xce, 0x3b, 0x9a, 0xca, 0x00,                              // val: 1000000000
        0xa8, 'd','u','r','a','t','i','o','n',                      // key: "duration"
        0xce, 0x1d, 0xcd, 0x65, 0x00,                              // val: 500000000
    };
    ddog_ByteSlice buffer = {
        .ptr = trace_payload,
        .len = sizeof(trace_payload)
    };
    ddog_TraceExporterResponse *response = NULL;

    // Send will deserialize the payload and attempt to forward it to the agent.
    // Without a running agent, this will fail with a network error.
    ret = ddog_trace_exporter_send(trace_exporter, buffer, &response);
    if (ret) {
        printf("Send returned expected error (no agent running): %s\n", ret->msg);
        ddog_trace_exporter_error_free(ret);
    } else {
        printf("Send succeeded\n");
        ddog_trace_exporter_response_free(response);
    }

    ddog_trace_exporter_free(trace_exporter);
    trace_exporter = NULL;
    ddog_trace_exporter_config_free(config);
    config = NULL;

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
