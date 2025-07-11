#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <datadog/common.h>
#include <datadog/profiling.h>
#include <memory>
#include <thread>
#include <iostream>
#include <string>
#include <chrono>

ddog_CharSlice to_slice_c_char(const char *s) {
  return ddog_CharSlice{.ptr = s, .len = strlen(s)};
}

struct ProfileDeleter {
  void operator()(ddog_prof_EncodedProfile *object) { ddog_prof_EncodedProfile_drop(&object); }
};

void print_error(const char *s, const ddog_prof_Profile_Error &err) {
  ddog_CharSlice message = ddog_prof_Profile_Error_message(err);
  printf("%s: %.*s\n", s, static_cast<int>(message.len), message.ptr);
}

int main(int argc, char **argv) {
  if (argc != 2) {
    printf("Usage: %s <service_name>\n", argv[0]);
    return 1;
  }

  // Create string table first
  ddog_prof_StringTable_NewResult strings_result = ddog_prof_StringTable_new();
  if (strings_result.tag != DDOG_PROF_STRING_TABLE_NEW_RESULT_OK) {
    print_error("Failed to create string table", strings_result.err);
    return 1;
  }
  ddog_prof_StringTable *strings = strings_result.ok;

  // Create profile builder
  ddog_prof_ValueType value_type = {
      .type = ddog_prof_StringTable_intern_utf8(strings, to_slice_c_char("wall-time")).ok,
      .unit = ddog_prof_StringTable_intern_utf8(strings, to_slice_c_char("nanoseconds")).ok,
  };

  ddog_prof_Slice_ValueType value_types = {
      .ptr = &value_type,
      .len = 1,
  };

  ddog_Timespec start = {
      .seconds = 0,
      .nanoseconds = 0,
  };

  ddog_prof_ProfileBuilder_NewResult builder_result = ddog_prof_ProfileBuilder_new(start);
  if (builder_result.tag != DDOG_PROF_PROFILE_BUILDER_NEW_RESULT_OK) {
    print_error("Failed to create profile builder", builder_result.err);
    ddog_prof_StringTable_drop(&strings);
    return 1;
  }
  ddog_prof_ProfileBuilder *builder = builder_result.ok;

  // Create function store
  ddog_prof_Store_Function *functions = ddog_prof_Store_Function_new();
  if (!functions) {
    printf("Failed to create function store\n");
    ddog_prof_StringTable_drop(&strings);
    ddog_prof_ProfileBuilder_drop(&builder);
    return 1;
  }

  // Add a function
  ddog_prof_Function function = {
      .id = 0,
      .name = ddog_prof_StringTable_intern_utf8(strings, to_slice_c_char("root")).ok,
      .system_name = ddog_prof_StringTable_intern_utf8(strings, to_slice_c_char("root")).ok,
      .filename = ddog_prof_StringTable_intern_utf8(strings, to_slice_c_char("root.cpp")).ok,
  };

  ddog_prof_Store_InsertResult add_func_result = ddog_prof_Store_Function_insert(functions, function);
  if (add_func_result.tag != DDOG_PROF_STORE_INSERT_RESULT_OK) {
    print_error("Failed to insert function", add_func_result.err);
    ddog_prof_Store_Function_drop(&functions);
    ddog_prof_StringTable_drop(&strings);
    ddog_prof_ProfileBuilder_drop(&builder);
    return 1;
  }

  // Create location store
  ddog_prof_Store_Location *locations = ddog_prof_Store_Location_new();
  if (!locations) {
    printf("Failed to create location store\n");
    ddog_prof_Store_Function_drop(&functions);
    ddog_prof_StringTable_drop(&strings);
    ddog_prof_ProfileBuilder_drop(&builder);
    return 1;
  }

  // Add a location
  ddog_prof_Line line = {
      .function_id = add_func_result.ok,
      .lineno = 42,
  };

  ddog_prof_Location location = {
      .id = 1,
      .mapping_id = 0,
      .address = 0,
      .line = line,
  };

  ddog_prof_Store_InsertResult add_loc_result = ddog_prof_Store_Location_insert(locations, location);
  if (add_loc_result.tag != DDOG_PROF_STORE_INSERT_RESULT_OK) {
    print_error("Failed to insert location", add_loc_result.err);
    ddog_prof_Store_Location_drop(&locations);
    ddog_prof_Store_Function_drop(&functions);
    ddog_prof_StringTable_drop(&strings);
    ddog_prof_ProfileBuilder_drop(&builder);
    return 1;
  }

  // Create sample manager
  ddog_prof_SampleManagerNewResult samples_result = ddog_prof_SampleManager_new(value_types);
  if (samples_result.tag != DDOG_PROF_SAMPLE_MANAGER_NEW_RESULT_OK) {
    print_error("Failed to create sample manager", samples_result.err);
    ddog_prof_Store_Location_drop(&locations);
    ddog_prof_Store_Function_drop(&functions);
    ddog_prof_StringTable_drop(&strings);
    ddog_prof_ProfileBuilder_drop(&builder);
    return 1;
  }
  ddog_prof_SampleManager *samples = samples_result.ok;

  // Create labels set
  ddog_prof_LabelsSet *labels_set = ddog_prof_LabelsSet_new();
  if (!labels_set) {
    printf("Failed to create labels set\n");
    ddog_prof_SampleManager_drop(&samples);
    ddog_prof_Store_Location_drop(&locations);
    ddog_prof_Store_Function_drop(&functions);
    ddog_prof_StringTable_drop(&strings);
    ddog_prof_ProfileBuilder_drop(&builder);
    return 1;
  }

  // Create stack traces
  ddog_prof_StackTraceSet *stack_traces = ddog_prof_StackTraceSet_new();
  if (!stack_traces) {
    printf("Failed to create stack traces\n");
    ddog_prof_LabelsSet_drop(&labels_set);
    ddog_prof_SampleManager_drop(&samples);
    ddog_prof_Store_Location_drop(&locations);
    ddog_prof_Store_Function_drop(&functions);
    ddog_prof_StringTable_drop(&strings);
    ddog_prof_ProfileBuilder_drop(&builder);
    return 1;
  }

  // Create endpoints
  ddog_prof_Endpoints *endpoints = ddog_prof_Endpoints_new();
  if (!endpoints) {
    printf("Failed to create endpoints\n");
    ddog_prof_StackTraceSet_drop(&stack_traces);
    ddog_prof_LabelsSet_drop(&labels_set);
    ddog_prof_SampleManager_drop(&samples);
    ddog_prof_Store_Location_drop(&locations);
    ddog_prof_Store_Function_drop(&functions);
    ddog_prof_StringTable_drop(&strings);
    ddog_prof_ProfileBuilder_drop(&builder);
    return 1;
  }

  // Add a stack trace
  const uint64_t locations_array[] = {1}; // Root location ID
  const ddog_Slice_U64 locations_slice = {
      .ptr = locations_array,
      .len = 1,
  };

  ddog_prof_LabelsSet_InsertResult stack_id_result = ddog_prof_StackTraceSet_insert(stack_traces, locations_slice);
  if (stack_id_result.tag != DDOG_PROF_LABELS_SET_INSERT_RESULT_OK) {
    print_error("Failed to insert stack trace", stack_id_result.err);
    ddog_prof_Endpoints_drop(&endpoints);
    ddog_prof_StackTraceSet_drop(&stack_traces);
    ddog_prof_LabelsSet_drop(&labels_set);
    ddog_prof_SampleManager_drop(&samples);
    ddog_prof_Store_Location_drop(&locations);
    ddog_prof_Store_Function_drop(&functions);
    ddog_prof_StringTable_drop(&strings);
    ddog_prof_ProfileBuilder_drop(&builder);
    return 1;
  }

  // Add a sample
  int64_t sample_value = 10000000; // 10ms
  ddog_Slice_I64 values = {
      .ptr = &sample_value,
      .len = 1,
  };

  ddog_prof_Sample sample = {
      .stack_trace_id = stack_id_result.ok,
      .values = values,
      .labels = {0},
      .timestamp = 0,
  };

  ddog_prof_Profile_VoidResult add_sample_result = ddog_prof_SampleManager_add_sample(samples, sample);
  if (add_sample_result.tag != DDOG_PROF_PROFILE_VOID_RESULT_OK) {
    print_error("Failed to add sample", add_sample_result.err);
    ddog_prof_Endpoints_drop(&endpoints);
    ddog_prof_StackTraceSet_drop(&stack_traces);
    ddog_prof_LabelsSet_drop(&labels_set);
    ddog_prof_SampleManager_drop(&samples);
    ddog_prof_Store_Location_drop(&locations);
    ddog_prof_Store_Function_drop(&functions);
    ddog_prof_StringTable_drop(&strings);
    ddog_prof_ProfileBuilder_drop(&builder);
    return 1;
  }

  // Add functions to the profile
  ddog_prof_Profile_VoidResult add_funcs_result = ddog_prof_ProfileBuilder_add_functions(builder, functions, strings);
  if (add_funcs_result.tag != DDOG_PROF_PROFILE_VOID_RESULT_OK) {
    print_error("Failed to add functions", add_funcs_result.err);
    ddog_prof_Endpoints_drop(&endpoints);
    ddog_prof_StackTraceSet_drop(&stack_traces);
    ddog_prof_LabelsSet_drop(&labels_set);
    ddog_prof_SampleManager_drop(&samples);
    ddog_prof_Store_Location_drop(&locations);
    ddog_prof_Store_Function_drop(&functions);
    ddog_prof_StringTable_drop(&strings);
    ddog_prof_ProfileBuilder_drop(&builder);
    return 1;
  }

  // Add locations to the profile
  ddog_prof_Profile_VoidResult add_locs_result = ddog_prof_ProfileBuilder_add_locations(builder, locations);
  if (add_locs_result.tag != DDOG_PROF_PROFILE_VOID_RESULT_OK) {
    print_error("Failed to add locations", add_locs_result.err);
    ddog_prof_Endpoints_drop(&endpoints);
    ddog_prof_StackTraceSet_drop(&stack_traces);
    ddog_prof_LabelsSet_drop(&labels_set);
    ddog_prof_SampleManager_drop(&samples);
    ddog_prof_Store_Location_drop(&locations);
    ddog_prof_Store_Function_drop(&functions);
    ddog_prof_StringTable_drop(&strings);
    ddog_prof_ProfileBuilder_drop(&builder);
    return 1;
  }

  // Add samples to the profile
  ddog_prof_Profile_VoidResult add_samples_result = ddog_prof_ProfileBuilder_add_samples(builder, samples, labels_set, strings, stack_traces, endpoints);
  if (add_samples_result.tag != DDOG_PROF_PROFILE_VOID_RESULT_OK) {
    print_error("Failed to add samples", add_samples_result.err);
    ddog_prof_Endpoints_drop(&endpoints);
    ddog_prof_StackTraceSet_drop(&stack_traces);
    ddog_prof_LabelsSet_drop(&labels_set);
    ddog_prof_SampleManager_drop(&samples);
    ddog_prof_Store_Location_drop(&locations);
    ddog_prof_Store_Function_drop(&functions);
    ddog_prof_StringTable_drop(&strings);
    ddog_prof_ProfileBuilder_drop(&builder);
    return 1;
  }

  // Build the profile
  ddog_prof_ProfileBuilder_BuildResult build_result = ddog_prof_ProfileBuilder_build(&builder, NULL);
  if (build_result.tag != DDOG_PROF_PROFILE_BUILDER_BUILD_RESULT_OK) {
    print_error("Failed to build profile", build_result.err);
    ddog_prof_Endpoints_drop(&endpoints);
    ddog_prof_StackTraceSet_drop(&stack_traces);
    ddog_prof_LabelsSet_drop(&labels_set);
    ddog_prof_SampleManager_drop(&samples);
    ddog_prof_Store_Location_drop(&locations);
    ddog_prof_Store_Function_drop(&functions);
    ddog_prof_StringTable_drop(&strings);
    ddog_prof_ProfileBuilder_drop(&builder);
    return 1;
  }

  // Create a request
  ddog_Vec_Tag tags = ddog_Vec_Tag_new();
  ddog_Vec_Tag_PushResult push_result = ddog_Vec_Tag_push(&tags, ddog_CharSlice{.ptr = "service", .len = 7}, to_slice_c_char(argv[1]));
  if (push_result.tag != DDOG_VEC_TAG_PUSH_RESULT_OK) {
    ddog_CharSlice message = ddog_Error_message(&push_result.err);
    printf("Failed to push tag: %.*s\n", static_cast<int>(message.len), message.ptr);
    ddog_Error_drop(&push_result.err);
    ddog_Vec_Tag_drop(tags);
    ddog_prof_Endpoints_drop(&endpoints);
    ddog_prof_StackTraceSet_drop(&stack_traces);
    ddog_prof_LabelsSet_drop(&labels_set);
    ddog_prof_SampleManager_drop(&samples);
    ddog_prof_Store_Location_drop(&locations);
    ddog_prof_Store_Function_drop(&functions);
    ddog_prof_StringTable_drop(&strings);
    return 1;
  }

  // Create an exporter
  ddog_prof_Slice_Exporter_File empty_file = ddog_prof_Exporter_Slice_File_empty();
  ddog_prof_ProfileExporter_Result exporter_result = ddog_prof_Exporter_new(
      ddog_CharSlice{.ptr = "dd-trace-cpp", .len = 11},
      ddog_CharSlice{.ptr = "1.0.0", .len = 5},
      ddog_CharSlice{.ptr = "cpp", .len = 3},
      &tags,
      ddog_prof_Endpoint_agentless(ddog_CharSlice{.ptr = "datadoghq.com", .len = 13}, ddog_CharSlice{.ptr = "api_key", .len = 7}));

  if (exporter_result.tag != DDOG_PROF_PROFILE_EXPORTER_RESULT_OK_HANDLE_PROFILE_EXPORTER) {
    ddog_CharSlice message = ddog_Error_message(&exporter_result.err);
    printf("Failed to create exporter: %.*s\n", static_cast<int>(message.len), message.ptr);
    ddog_Error_drop(&exporter_result.err);
    ddog_Vec_Tag_drop(tags);
    ddog_prof_Endpoints_drop(&endpoints);
    ddog_prof_StackTraceSet_drop(&stack_traces);
    ddog_prof_LabelsSet_drop(&labels_set);
    ddog_prof_SampleManager_drop(&samples);
    ddog_prof_Store_Location_drop(&locations);
    ddog_prof_Store_Function_drop(&functions);
    ddog_prof_StringTable_drop(&strings);
    return 1;
  }

  ddog_prof_ProfileExporter exporter = exporter_result.ok;

  // Build request
  ddog_prof_Request_Result request_result = ddog_prof_Exporter_Request_build(
      &exporter,
      build_result.ok,
      empty_file,
      empty_file,
      NULL,
      NULL,
      NULL);

  if (request_result.tag != DDOG_PROF_REQUEST_RESULT_OK_HANDLE_REQUEST) {
    ddog_CharSlice message = ddog_Error_message(&request_result.err);
    printf("Failed to build request: %.*s\n", static_cast<int>(message.len), message.ptr);
    ddog_Error_drop(&request_result.err);
    ddog_prof_Exporter_drop(&exporter);
    ddog_Vec_Tag_drop(tags);
    ddog_prof_Endpoints_drop(&endpoints);
    ddog_prof_StackTraceSet_drop(&stack_traces);
    ddog_prof_LabelsSet_drop(&labels_set);
    ddog_prof_SampleManager_drop(&samples);
    ddog_prof_Store_Location_drop(&locations);
    ddog_prof_Store_Function_drop(&functions);
    ddog_prof_StringTable_drop(&strings);
    return 1;
  }

  // Send request
  ddog_CancellationToken cancel = ddog_CancellationToken_new();
  ddog_prof_Result_HttpStatus send_result = ddog_prof_Exporter_send(&exporter, &request_result.ok, &cancel);
  ddog_CancellationToken_drop(&cancel);

  if (send_result.tag != DDOG_PROF_RESULT_HTTP_STATUS_OK_HTTP_STATUS) {
    ddog_CharSlice message = ddog_Error_message(&send_result.err);
    printf("Failed to send request: %.*s\n", static_cast<int>(message.len), message.ptr);
    ddog_Error_drop(&send_result.err);
    ddog_prof_Exporter_drop(&exporter);
    ddog_Vec_Tag_drop(tags);
    ddog_prof_Endpoints_drop(&endpoints);
    ddog_prof_StackTraceSet_drop(&stack_traces);
    ddog_prof_LabelsSet_drop(&labels_set);
    ddog_prof_SampleManager_drop(&samples);
    ddog_prof_Store_Location_drop(&locations);
    ddog_prof_Store_Function_drop(&functions);
    ddog_prof_StringTable_drop(&strings);
    return 1;
  }

  printf("Profile sent successfully (HTTP %d)\n", send_result.ok.code);

  // Clean up
  ddog_prof_Exporter_drop(&exporter);
  ddog_Vec_Tag_drop(tags);
  ddog_prof_Endpoints_drop(&endpoints);
  ddog_prof_StackTraceSet_drop(&stack_traces);
  ddog_prof_LabelsSet_drop(&labels_set);
  ddog_prof_SampleManager_drop(&samples);
  ddog_prof_Store_Location_drop(&locations);
  ddog_prof_Store_Function_drop(&functions);
  ddog_prof_StringTable_drop(&strings);

  return 0;
}
