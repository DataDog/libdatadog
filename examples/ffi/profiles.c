// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// Basic FFI example using the revamped profiling APIs
#include <datadog/common.h>
#include <datadog/profiling.h>
#include <stdio.h>
#include <stdlib.h>

static void check_ok(struct ddog_prof_Status status, const char *context) {
  if (status.flags != 0) {
    const char *msg = status.err ? status.err : "(unknown)";
    fprintf(stderr, "%s: %s\n", context, msg);
    ddog_prof_Status_drop(&status);
    // this will cause leaks but this is just an example.
    exit(EXIT_FAILURE);
  }
}

int main(void) {
  // Create core handles
  ddog_prof_ProfilesDictionaryHandle dict = NULL;
  check_ok(ddog_prof_ProfilesDictionary_new(&dict), "ProfilesDictionary_new");

  ddog_prof_ScratchPadHandle scratch = NULL;
  check_ok(ddog_prof_ScratchPad_new(&scratch), "ScratchPad_new");

  // Prepare StringIds for a ValueType (wall-time / nanoseconds)
  ddog_prof_StringId vt_type = {0};
  ddog_prof_StringId vt_unit = {0};
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&vt_type, dict, DDOG_CHARSLICE_C("wall-time"), DDOG_PROF_UTF8_OPTION_VALIDATE),
           "ProfilesDictionary_insert_str(type)");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&vt_unit, dict, DDOG_CHARSLICE_C("nanoseconds"), DDOG_PROF_UTF8_OPTION_VALIDATE),
           "ProfilesDictionary_insert_str(unit)");
  ddog_prof_ValueType vt = { .type_id = vt_type, .unit_id = vt_unit };

  // Insert function/mapping strings and create ids
  ddog_prof_StringId fn_name = {0}, fn_sys = {0}, fn_file = {0};
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&fn_name, dict, DDOG_CHARSLICE_C("{main}"), DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(fn name)");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&fn_sys, dict, DDOG_CHARSLICE_C("{main}"), DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(fn system)");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&fn_file, dict, DDOG_CHARSLICE_C("/srv/example/index.php"), DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(fn file)");

  ddog_prof_Function func = { .name = fn_name, .system_name = fn_sys, .file_name = fn_file };
  ddog_prof_FunctionId func_id = NULL;
  check_ok(ddog_prof_ProfilesDictionary_insert_function(&func_id, dict, &func), "insert_function");

  ddog_prof_StringId map_file = {0}, map_build = {0};
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&map_file, dict, DDOG_CHARSLICE_C("/bin/example"), DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(map filename)");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&map_build, dict, DDOG_CHARSLICE_C("deadbeef"), DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(map build)");
  ddog_prof_Mapping mapping = {
    .memory_start = 0,
    .memory_limit = 0,
    .file_offset = 0,
    .filename = map_file,
    .build_id = map_build,
  };
  ddog_prof_MappingId map_id = NULL;
  check_ok(ddog_prof_ProfilesDictionary_insert_mapping(&map_id, dict, &mapping), "insert_mapping");

  // Create a location in the scratchpad
  ddog_prof_Option_FunctionId opt_fn = { .tag = DDOG_PROF_OPTION_FUNCTION_ID_SOME_FUNCTION_ID, .some = func_id };
  ddog_prof_Option_MappingId opt_map = { .tag = DDOG_PROF_OPTION_MAPPING_ID_SOME_MAPPING_ID, .some = map_id };
  ddog_prof_Line line = { .line_number = 0, .function_id = opt_fn };
  ddog_prof_Location loc = { .address = 0, .mapping_id = opt_map, .line = line };
  ddog_prof_LocationId loc_id = NULL;
  check_ok(ddog_prof_ScratchPad_insert_location(&loc_id, scratch, &loc), "ScratchPad_insert_location");

  // Create a stack consisting of just that one location
  ddog_prof_LocationId locs[1] = { loc_id };
  ddog_prof_Slice_LocationId loc_slice = { .ptr = locs, .len = 1 };
  ddog_prof_StackId stack_id = {0};
  check_ok(ddog_prof_ScratchPad_insert_stack(&stack_id, scratch, loc_slice), "ScratchPad_insert_stack");

  // Create a profile and add basic metadata
  ddog_prof_ProfileHandle profile = NULL;
  check_ok(ddog_prof_Profile_new(&profile), "Profile_new");
  check_ok(ddog_prof_Profile_add_sample_type(profile, vt), "Profile_add_sample_type");
  check_ok(ddog_prof_Profile_add_period(profile, 1000000000LL, vt), "Profile_add_period");

  // Build a single sample via SampleBuilder
  ddog_prof_SampleBuilderHandle sb = NULL;
  check_ok(ddog_prof_SampleBuilder_new(&sb, scratch), "SampleBuilder_new");
  check_ok(ddog_prof_SampleBuilder_stack_id(sb, stack_id), "SampleBuilder_stack_id");
  check_ok(ddog_prof_SampleBuilder_value(sb, 10), "SampleBuilder_value");
  check_ok(ddog_prof_SampleBuilder_attribute_str(sb,
                                                 DDOG_CHARSLICE_C("unique_counter"),
                                                 DDOG_CHARSLICE_C("1"),
                                                 DDOG_PROF_UTF8_OPTION_VALIDATE),
           "SampleBuilder_attribute_str");
  check_ok(ddog_prof_SampleBuilder_build_into_profile(&sb, profile), "SampleBuilder_build_into_profile");

  // Build a pprof using PprofBuilder
  ddog_prof_PprofBuilderHandle pprof = NULL;
  check_ok(ddog_prof_PprofBuilder_new(&pprof, dict, scratch), "PprofBuilder_new");
  check_ok(ddog_prof_PprofBuilder_try_add_profile(pprof, profile), "PprofBuilder_try_add_profile");

  // Build an uncompressed pprof into an EncodedProfile handle
  ddog_prof_EncodedProfile encoded = {0};
  struct ddog_Timespec start = { .seconds = 0, .nanoseconds = 0 };
  struct ddog_Timespec end = { .seconds = 1, .nanoseconds = 0 };
  check_ok(ddog_prof_PprofBuilder_build_uncompressed(&encoded, pprof, 4096, start, end),
           "PprofBuilder_build_uncompressed");

  // Normally, you would now build an Exporter Request that consumes `encoded`.
  // For this example, we stop here after exercising the core APIs.

  // Cleanup
  ddog_prof_PprofBuilder_drop(&pprof);
  ddog_prof_Profile_drop(&profile);
  ddog_prof_ScratchPad_drop(&scratch);
  ddog_prof_ProfilesDictionary_drop(&dict);
  return 0;
}
