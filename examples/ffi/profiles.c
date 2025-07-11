// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/common.h>
#include <datadog/profiling.h>
#include <stdio.h>
#include <stdlib.h>

int main(void) {
  // Create value types for the profile
  const ddog_prof_ValueType wall_time = {
      .type_ = DDOG_CHARSLICE_C("wall-time"),
      .unit = DDOG_CHARSLICE_C("nanoseconds"),
  };
  const ddog_prof_Slice_ValueType sample_types = {&wall_time, 1};

  // Create a new ProfileBuilder
  ddog_prof_ProfileBuilderNewResult builder_result = ddog_prof_ProfileBuilder_new();
  if (builder_result.tag != DDOG_PROF_PROFILEBUILDER_NEW_RESULT_OK) {
    ddog_CharSlice message = ddog_Error_message(&builder_result.err);
    fprintf(stderr, "Failed to create ProfileBuilder: %.*s\n", (int)message.len, message.ptr);
    ddog_Error_drop(&builder_result.err);
    exit(EXIT_FAILURE);
  }

  ddog_prof_ProfileBuilder *builder = &builder_result.ok;

  // Create string table for function names and filenames
  ddog_prof_StringTable *strings = ddog_prof_StringTable_new();
  if (!strings) {
    fprintf(stderr, "Failed to create string table\n");
    exit(EXIT_FAILURE);
  }

  // Create function store
  ddog_prof_Store_Function *functions = ddog_prof_Store_Function_new();
  if (!functions) {
    fprintf(stderr, "Failed to create function store\n");
    exit(EXIT_FAILURE);
  }

  // Create a function
  ddog_prof_Function root_function = {
      .name = DDOG_CHARSLICE_C("{main}"),
      .filename = DDOG_CHARSLICE_C("/srv/example/index.php"),
  };

  // Add function to store
  ddog_prof_Store_Function_Result add_func_result = ddog_prof_Store_Function_add(functions, root_function);
  if (add_func_result.tag != DDOG_PROF_STORE_FUNCTION_RESULT_OK) {
    fprintf(stderr, "Failed to add function to store\n");
    exit(EXIT_FAILURE);
  }

  // Create location store
  ddog_prof_Store_Location *locations = ddog_prof_Store_Location_new();
  if (!locations) {
    fprintf(stderr, "Failed to create location store\n");
    exit(EXIT_FAILURE);
  }

  // Create a location
  ddog_prof_Location root_location = {
      .mapping = (ddog_prof_Mapping){0},  // zero-initialized mapping is valid
      .function = root_function,
  };

  // Add location to store
  ddog_prof_Store_Location_Result add_loc_result = ddog_prof_Store_Location_add(locations, root_location);
  if (add_loc_result.tag != DDOG_PROF_STORE_LOCATION_RESULT_OK) {
    fprintf(stderr, "Failed to add location to store\n");
    exit(EXIT_FAILURE);
  }

  // Create sample manager
  ddog_prof_SampleManagerNewResult sample_mgr_result = ddog_prof_SampleManager_new(sample_types);
  if (sample_mgr_result.tag != DDOG_PROF_SAMPLEMANAGER_NEW_RESULT_OK) {
    fprintf(stderr, "Failed to create sample manager\n");
    exit(EXIT_FAILURE);
  }

  ddog_prof_SampleManager *samples = &sample_mgr_result.ok;

  // Create labels set
  ddog_prof_LabelsSet *labels_set = ddog_prof_LabelsSet_new();
  if (!labels_set) {
    fprintf(stderr, "Failed to create labels set\n");
    exit(EXIT_FAILURE);
  }

  // Create stack traces set
  ddog_prof_SliceSet_U64 *stack_traces = ddog_prof_SliceSet_U64_new();
  if (!stack_traces) {
    fprintf(stderr, "Failed to create stack traces set\n");
    exit(EXIT_FAILURE);
  }

  // Create endpoints
  ddog_prof_Endpoints *endpoints = ddog_prof_Endpoints_new();
  if (!endpoints) {
    fprintf(stderr, "Failed to create endpoints\n");
    exit(EXIT_FAILURE);
  }

  // Create compressor
  ddog_prof_Compressor *compressor = ddog_prof_Compressor_new();
  if (!compressor) {
    fprintf(stderr, "Failed to create compressor\n");
    exit(EXIT_FAILURE);
  }

  // Add components to the builder
  ddog_prof_Profile_VoidResult add_funcs_result = ddog_prof_ProfileBuilder_add_functions(builder, functions, strings, compressor);
  if (add_funcs_result.tag != DDOG_PROF_PROFILE_VOID_RESULT_OK) {
    fprintf(stderr, "Failed to add functions to profile\n");
    exit(EXIT_FAILURE);
  }

  ddog_prof_Profile_VoidResult add_locs_result = ddog_prof_ProfileBuilder_add_locations(builder, locations, compressor);
  if (add_locs_result.tag != DDOG_PROF_PROFILE_VOID_RESULT_OK) {
    fprintf(stderr, "Failed to add locations to profile\n");
    exit(EXIT_FAILURE);
  }

  // Add sample data
  int64_t value = 10;
  ddog_prof_Label label = {
      .key = DDOG_CHARSLICE_C("unique_counter"),
      .num = 0,
  };

  for (int i = 0; i < 10000000; i++) {
    label.num = i;

    // Add sample to manager
    ddog_prof_Sample sample = {
        .locations = {&root_location, 1},
        .values = {&value, 1},
        .labels = {&label, 1},
    };

    ddog_prof_SampleManager_Result add_sample_result = ddog_prof_SampleManager_add(samples, sample);
    if (add_sample_result.tag != DDOG_PROF_SAMPLEMANAGER_RESULT_OK) {
      fprintf(stderr, "Failed to add sample\n");
      continue;
    }
  }

  // Add samples to builder
  ddog_prof_Profile_VoidResult add_samples_result = ddog_prof_ProfileBuilder_add_samples(
      builder, samples, labels_set, strings, stack_traces, endpoints, compressor);
  if (add_samples_result.tag != DDOG_PROF_PROFILE_VOID_RESULT_OK) {
    fprintf(stderr, "Failed to add samples to profile\n");
    exit(EXIT_FAILURE);
  }

  // Build the final profile
  ddog_prof_ProfileBuilderBuildResult build_result = ddog_prof_ProfileBuilder_build(&builder, NULL);
  if (build_result.tag != DDOG_PROF_PROFILEBUILDER_BUILD_RESULT_OK) {
    fprintf(stderr, "Failed to build profile\n");
    exit(EXIT_FAILURE);
  }

  // Clean up
  ddog_prof_ProfileBuilder_drop(&builder);
  ddog_prof_StringTable_drop(&strings);
  ddog_prof_Store_Function_drop(&functions);
  ddog_prof_Store_Location_drop(&locations);
  ddog_prof_SampleManager_drop(&samples);
  ddog_prof_LabelsSet_drop(&labels_set);
  ddog_prof_SliceSet_U64_drop(&stack_traces);
  ddog_prof_Endpoints_drop(&endpoints);
  ddog_prof_Compressor_drop(&compressor);

  return 0;
}
