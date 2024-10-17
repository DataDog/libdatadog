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
    ddog_TraceExporterConfig* trace_exporter_config;
    ddog_CharSlice url = DDOG_CHARSLICE_C("http://localhost:8126/");
    ddog_CharSlice tracer_version = DDOG_CHARSLICE_C("v0.1");
    ddog_CharSlice language = DDOG_CHARSLICE_C("dotnet");
    ddog_CharSlice language_version = DDOG_CHARSLICE_C("10.0");
    ddog_CharSlice language_interpreter = DDOG_CHARSLICE_C("X");
    ddog_CharSlice hostname = DDOG_CHARSLICE_C("host1");
    ddog_CharSlice env = DDOG_CHARSLICE_C("staging");
    ddog_CharSlice version = DDOG_CHARSLICE_C("1.0");
    ddog_CharSlice service = DDOG_CHARSLICE_C("test_app");

    TRY(ddog_trace_exporter_config_new(&trace_exporter_config));

    TRY(ddog_trace_exporter_config_set_option(trace_exporter_config, (struct ddog_TraceExporterConfigOption) {
                .tag = DDOG_TRACE_EXPORTER_CONFIG_OPTION_URL, .url = url }));

    TRY(ddog_trace_exporter_config_set_option(trace_exporter_config, (struct ddog_TraceExporterConfigOption) {
                .tag = DDOG_TRACE_EXPORTER_CONFIG_OPTION_URL, .language = language }));

    TRY(ddog_trace_exporter_config_set_option(trace_exporter_config, (struct ddog_TraceExporterConfigOption) {
        .tag = DDOG_TRACE_EXPORTER_CONFIG_OPTION_TRACER_VERSION, .language = tracer_version }));

    TRY(ddog_trace_exporter_config_set_option(trace_exporter_config, (struct ddog_TraceExporterConfigOption) {
        .tag = DDOG_TRACE_EXPORTER_CONFIG_OPTION_LANGUAGE_INTERPRETER, .language = language_interpreter }));

    TRY(ddog_trace_exporter_config_set_option(trace_exporter_config, (struct ddog_TraceExporterConfigOption) {
        .tag = DDOG_TRACE_EXPORTER_CONFIG_OPTION_LANGUAGE_VERSION, .language = language_version }));

    TRY(ddog_trace_exporter_config_set_option(trace_exporter_config, (struct ddog_TraceExporterConfigOption) {
        .tag = DDOG_TRACE_EXPORTER_CONFIG_OPTION_HOSTNAME, .language = hostname }));

    TRY(ddog_trace_exporter_config_set_option(trace_exporter_config, (struct ddog_TraceExporterConfigOption) {
        .tag = DDOG_TRACE_EXPORTER_CONFIG_OPTION_ENV, .language = env }));

    TRY(ddog_trace_exporter_config_set_option(trace_exporter_config, (struct ddog_TraceExporterConfigOption) {
        .tag = DDOG_TRACE_EXPORTER_CONFIG_OPTION_VERSION, .language = version }));

    TRY(ddog_trace_exporter_config_set_option(trace_exporter_config, (struct ddog_TraceExporterConfigOption) {
        .tag = DDOG_TRACE_EXPORTER_CONFIG_OPTION_SERVICE, .language = service }));

    TRY(ddog_trace_exporter_new(&trace_exporter, trace_exporter_config));

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
