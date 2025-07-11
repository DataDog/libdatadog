// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

extern "C" {
#include <datadog/common.h>
#include <datadog/crashtracker.h>
#include <datadog/profiling.h>
}
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <memory>
#include <optional>
#include <string>
#include <thread>
#include <vector>

extern "C" void ddog_prof_EncodedProfile_drop(ddog_prof_EncodedProfile **profile);

static ddog_CharSlice to_slice_c_char(const char *s) { return {.ptr = s, .len = strlen(s)}; }
static ddog_CharSlice to_slice_c_char(const char *s, std::size_t size) {
  return {.ptr = s, .len = size};
}
static ddog_CharSlice to_slice_string(std::string const &s) {
  return {.ptr = s.data(), .len = s.length()};
}

static std::string to_string(ddog_CharSlice s) {
  return std::string(s.ptr, s.len);
}

static void print_error(ddog_prof_Profile_Error err) {
  ddog_CharSlice message = ddog_prof_Profile_Error_message(err);
  printf("Error: %.*s\n", static_cast<int>(message.len), message.ptr);
}

struct ProfileDeleter {
  void operator()(ddog_prof_EncodedProfile *object) { ddog_prof_EncodedProfile_drop(&object); }
};

int main(void) {
  // Create string table first
  ddog_prof_StringTable_NewResult strings_result = ddog_prof_StringTable_new();
  if (strings_result.tag != DDOG_PROF_STRING_TABLE_NEW_RESULT_OK) {
    print_error(strings_result.err);
    return 1;
  }
  ddog_prof_StringTable *strings = strings_result.ok;

  // Create profile builder
  auto start = ddog_Timespec{.seconds = 1234567890, .nanoseconds = 123456789};
  ddog_prof_ProfileBuilder_NewResult builder_result = ddog_prof_ProfileBuilder_new(start);
  if (builder_result.tag != DDOG_PROF_PROFILE_BUILDER_NEW_RESULT_OK) {
    print_error(builder_result.err);
    ddog_prof_StringTable_drop(&strings);
    return 1;
  }
  ddog_prof_ProfileBuilder *builder = builder_result.ok;

  // Create function store
  ddog_prof_Store_Function *functions = new ddog_prof_Store_Function;
  ddog_prof_Function function = {
      .id = 1,
      .name = {.offset = 0},
      .system_name = {.offset = 0},
      .filename = {.offset = 0},
  };
  auto add_func_result = ddog_prof_Store_Function_insert(functions, function);
  if (add_func_result.tag != DDOG_PROF_STORE_INSERT_RESULT_OK) {
    print_error(add_func_result.err);
    ddog_prof_ProfileBuilder_drop(&builder);
    ddog_prof_StringTable_drop(&strings);
    delete functions;
    return 1;
  }

  // Create location store
  ddog_prof_Store_Location *locations = new ddog_prof_Store_Location;
  ddog_prof_Location location = {
      .id = 1,
      .mapping_id = 0,
      .address = 0x1234567890,
      .line = {.function_id = 1, .lineno = 42},
  };
  auto add_loc_result = ddog_prof_Store_Location_insert(locations, location);
  if (add_loc_result.tag != DDOG_PROF_STORE_INSERT_RESULT_ERR) {
    print_error(add_loc_result.err);
    ddog_prof_ProfileBuilder_drop(&builder);
    ddog_prof_StringTable_drop(&strings);
    delete functions;
    delete locations;
    return 1;
  }

  // Create sample manager
  ddog_prof_ValueType sample_types[] = {
      {.type = {.offset = 0}, .unit = {.offset = 0}},
  };
  auto samples_result = ddog_prof_SampleManager_new({.ptr = sample_types, .len = 1});
  if (samples_result.tag != DDOG_PROF_SAMPLE_MANAGER_NEW_RESULT_OK) {
    print_error(samples_result.err);
    ddog_prof_ProfileBuilder_drop(&builder);
    ddog_prof_StringTable_drop(&strings);
    delete functions;
    delete locations;
    return 1;
  }
  ddog_prof_SampleManager *samples = samples_result.ok;

  // Create stack traces
  ddog_prof_StackTraceSet *stack_traces = new ddog_prof_StackTraceSet;
  uint64_t stack_trace[] = {1};
  auto stack_id_result = ddog_prof_StackTraceSet_insert(stack_traces, {.ptr = stack_trace, .len = 1});
  if (stack_id_result.tag != DDOG_PROF_LABELS_SET_INSERT_RESULT_OK) {
    print_error(stack_id_result.err);
    ddog_prof_ProfileBuilder_drop(&builder);
    ddog_prof_StringTable_drop(&strings);
    delete functions;
    delete locations;
    ddog_prof_SampleManager_drop(&samples);
    delete stack_traces;
    return 1;
  }

  // Add a sample
  int64_t values[] = {1};
  ddog_prof_Sample sample = {
      .stack_trace_id = stack_id_result.ok,
      .values = {.ptr = values, .len = 1},
      .labels = {.opaque = 0},
      .timestamp = 0,
  };
  auto add_sample_result = ddog_prof_SampleManager_add_sample(samples, sample);
  if (add_sample_result.tag != DDOG_PROF_PROFILE_VOID_RESULT_OK) {
    print_error(add_sample_result.err);
    ddog_prof_ProfileBuilder_drop(&builder);
    ddog_prof_StringTable_drop(&strings);
    delete functions;
    delete locations;
    ddog_prof_SampleManager_drop(&samples);
    delete stack_traces;
    return 1;
  }

  // Add functions to profile
  auto add_funcs_result = ddog_prof_ProfileBuilder_add_functions(builder, functions, strings);
  if (add_funcs_result.tag != DDOG_PROF_PROFILE_VOID_RESULT_OK) {
    print_error(add_funcs_result.err);
    ddog_prof_ProfileBuilder_drop(&builder);
    ddog_prof_StringTable_drop(&strings);
    delete functions;
    delete locations;
    ddog_prof_SampleManager_drop(&samples);
    delete stack_traces;
    return 1;
  }

  // Add locations to profile
  auto add_locs_result = ddog_prof_ProfileBuilder_add_locations(builder, locations);
  if (add_locs_result.tag != DDOG_PROF_PROFILE_VOID_RESULT_OK) {
    print_error(add_locs_result.err);
    ddog_prof_ProfileBuilder_drop(&builder);
    ddog_prof_StringTable_drop(&strings);
    delete functions;
    delete locations;
    ddog_prof_SampleManager_drop(&samples);
    delete stack_traces;
    return 1;
  }

  // Add samples to profile
  auto add_samples_result = ddog_prof_ProfileBuilder_add_samples(builder, samples, nullptr, strings, stack_traces, nullptr);
  if (add_samples_result.tag != DDOG_PROF_PROFILE_VOID_RESULT_OK) {
    print_error(add_samples_result.err);
    ddog_prof_ProfileBuilder_drop(&builder);
    ddog_prof_StringTable_drop(&strings);
    delete functions;
    delete locations;
    ddog_prof_SampleManager_drop(&samples);
    delete stack_traces;
    return 1;
  }

  // Build profile
  auto end = ddog_Timespec{.seconds = 1234567890, .nanoseconds = 123456789};
  auto build_result = ddog_prof_ProfileBuilder_build(&builder, &end);
  if (build_result.tag != DDOG_PROF_PROFILE_BUILDER_BUILD_RESULT_OK) {
    print_error(build_result.err);
    ddog_prof_ProfileBuilder_drop(&builder);
    ddog_prof_StringTable_drop(&strings);
    delete functions;
    delete locations;
    ddog_prof_SampleManager_drop(&samples);
    delete stack_traces;
    return 1;
  }

  // Create exporter
  ddog_Vec_Tag tags = ddog_Vec_Tag_new();
  ddog_prof_Endpoint endpoint = ddog_prof_Endpoint_agent(to_slice_c_char("http://localhost:8126"));
  auto exporter_result = ddog_prof_Exporter_new(
      to_slice_c_char("dd-trace-cpp"),
      to_slice_c_char("1.0.0"),
      to_slice_c_char("cpp"),
      &tags,
      endpoint);
  if (exporter_result.tag != DDOG_PROF_PROFILE_EXPORTER_RESULT_OK_HANDLE_PROFILE_EXPORTER) {
    print_error(exporter_result.err);
    ddog_prof_ProfileBuilder_drop(&builder);
    ddog_prof_StringTable_drop(&strings);
    delete functions;
    delete locations;
    ddog_prof_SampleManager_drop(&samples);
    delete stack_traces;
    ddog_Vec_Tag_drop(tags);
    return 1;
  }
  ddog_prof_ProfileExporter exporter = exporter_result.ok;

  // Create request
  auto request_result = ddog_prof_Exporter_Request_build(
      &exporter,
      build_result.ok,
      ddog_prof_Slice_Exporter_File{.ptr = nullptr, .len = 0},
      ddog_prof_Slice_Exporter_File{.ptr = nullptr, .len = 0},
      nullptr,
      nullptr,
      nullptr);
  if (request_result.tag != DDOG_PROF_REQUEST_RESULT_OK_HANDLE_REQUEST) {
    print_error(request_result.err);
    ddog_prof_ProfileBuilder_drop(&builder);
    ddog_prof_StringTable_drop(&strings);
    delete functions;
    delete locations;
    ddog_prof_SampleManager_drop(&samples);
    delete stack_traces;
    ddog_Vec_Tag_drop(tags);
    return 1;
  }
  ddog_prof_Request request = request_result.ok;

  // Send request
  auto send_result = ddog_prof_Exporter_send(&exporter, &request, nullptr);
  if (send_result.tag != DDOG_PROF_RESULT_HTTP_STATUS_OK_HTTP_STATUS) {
    print_error(send_result.err);
    ddog_prof_ProfileBuilder_drop(&builder);
    ddog_prof_StringTable_drop(&strings);
    delete functions;
    delete locations;
    ddog_prof_SampleManager_drop(&samples);
    delete stack_traces;
    ddog_Vec_Tag_drop(tags);
    return 1;
  }

  // Clean up
  ddog_prof_Exporter_Request_drop(&request);
  ddog_prof_EncodedProfile_drop(&build_result.ok);
  ddog_prof_StringTable_drop(&strings);
  delete functions;
  delete locations;
  ddog_prof_SampleManager_drop(&samples);
  delete stack_traces;
  ddog_Vec_Tag_drop(tags);

  return 0;
}
