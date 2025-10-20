// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0this example weights in a loop

extern "C" {
#include <datadog/common.h>
#include <datadog/profiling.h>
}
#include <cstdio>
#include <cstdlib>
#include <cstring>

static ddog_CharSlice to_slice_c_char(const char *s) {
  return ddog_CharSlice{.ptr = s, .len = strlen(s)};
}

static void check_ok(ddog_prof_Status status, const char *ctx) {
  if (status.flags != 0) {
    const char *msg = status.err ? status.err : "(unknown)";
    fprintf(stderr, "%s: %s\n", ctx, msg);
    ddog_prof_Status_drop(&status);
    // this will cause leaks but this is just an example.
    exit(EXIT_FAILURE);
  }
}

int main(int argc, char **argv) {
  if (argc != 2) {
    printf("Usage: %s <service_name>\n", argv[0]);
    return 1;
  }

  const char *api_key = getenv("DD_API_KEY");
  if (!api_key) {
    printf("DD_API_KEY environment variable is no set\n");
    return 1;
  }

  // Core handles
  ddog_prof_ProfilesDictionaryHandle dict = NULL;
  check_ok(ddog_prof_ProfilesDictionary_new(&dict), "ProfilesDictionary_new");

  ddog_prof_ScratchPadHandle scratch = NULL;
  check_ok(ddog_prof_ScratchPad_new(&scratch), "ScratchPad_new");

  // ValueType: wall-time / nanoseconds
  ddog_prof_ValueType vt = {DDOG_PROF_STRINGID_EMPTY, DDOG_PROF_STRINGID_EMPTY};
  const ddog_CharSlice wall_time = DDOG_CHARSLICE_C_BARE("wall-time");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&vt.type_id, dict, wall_time,
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(type)");
  const ddog_CharSlice nanoseconds = DDOG_CHARSLICE_C_BARE("nanoseconds");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&vt.unit_id, dict, nanoseconds,
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(unit)");

  // Insert a function and mapping in the dictionary
  ddog_prof_Function func = {.name = DDOG_PROF_STRINGID_EMPTY,
                             .system_name = DDOG_PROF_STRINGID_EMPTY,
                             .file_name = DDOG_PROF_STRINGID_EMPTY};
  const ddog_CharSlice root_str = DDOG_CHARSLICE_C_BARE("<?php");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&func.name, dict, root_str,
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(fn name)");
  const ddog_CharSlice filename = DDOG_CHARSLICE_C_BARE("/srv/example/index.php");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&func.file_name, dict, filename,
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(fn file)");
  ddog_prof_FunctionId func_id = NULL;
  check_ok(ddog_prof_ProfilesDictionary_insert_function(&func_id, dict, &func), "insert_function");

  // Insert a location and stack in the scratchpad
  ddog_prof_Line line = {.line_number = 42, .function_id = func_id};
  // No mapping id is valid, used in dynamic languages.
  ddog_prof_Location loc = {.address = 0, .mapping_id = NULL, .line = line};
  ddog_prof_LocationId locs[1] = {NULL};
  check_ok(ddog_prof_ScratchPad_insert_location(locs, scratch, &loc), "ScratchPad_insert_location");
  ddog_prof_Slice_LocationId loc_slice = {.ptr = locs, .len = 1};
  ddog_prof_StackId stack_id = {0};
  check_ok(ddog_prof_ScratchPad_insert_stack(&stack_id, scratch, loc_slice),
           "ScratchPad_insert_stack");

  // Create a profile and add sample type + period
  ddog_prof_ProfileHandle profile = NULL;
  check_ok(ddog_prof_Profile_new(&profile), "Profile_new");
  check_ok(ddog_prof_Profile_add_sample_type(profile, vt), "Profile_add_sample_type");
  check_ok(ddog_prof_Profile_add_period(profile, 10'000'000LL, vt),
           "Profile_add_period"); // 10ms tick

  // Build one sample via SampleBuilder with label language:php.
  ddog_prof_StringId language_id = DDOG_PROF_STRINGID_EMPTY;
  ddog_CharSlice language = DDOG_CHARSLICE_C_BARE("language");
  ddog_CharSlice language_php = DDOG_CHARSLICE_C_BARE("php");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&language_id, dict, language,
                                                   DDOG_PROF_UTF8_OPTION_ASSUME),
           "insert_str(sample label)");

  ddog_prof_SampleBuilderHandle sb = NULL;
  check_ok(ddog_prof_SampleBuilder_new(&sb, profile, scratch), "SampleBuilder_new");
  check_ok(ddog_prof_SampleBuilder_stack_id(sb, stack_id), "SampleBuilder_stack_id");
  check_ok(ddog_prof_SampleBuilder_value(sb, 10'000'000LL), "SampleBuilder_value");
  check_ok(ddog_prof_SampleBuilder_attribute_str(sb, language_id, language_php,
                                                 DDOG_PROF_UTF8_OPTION_ASSUME),
           "SampleBuilder_attribute_str");
  check_ok(ddog_prof_SampleBuilder_finish(&sb), "SampleBuilder_finish");

  // Build EncodedProfile with PprofBuilder
  ddog_prof_PprofBuilderHandle pprof = NULL;
  check_ok(ddog_prof_PprofBuilder_new(&pprof, dict, scratch), "PprofBuilder_new");
  check_ok(ddog_prof_PprofBuilder_add_profile(pprof, profile), "PprofBuilder_add_profile");
  ddog_prof_EncodedProfile encoded = {0};
  ddog_Timespec start = {.seconds = 0, .nanoseconds = 0};
  ddog_Timespec end = {.seconds = 1, .nanoseconds = 0};
  check_ok(ddog_prof_PprofBuilder_build_uncompressed(&encoded, pprof, 4096, start, end),
           "PprofBuilder_build_uncompressed");

  // Build and send exporter request
  ddog_Vec_Tag tags = ddog_Vec_Tag_new();
  const ddog_CharSlice service_key = DDOG_CHARSLICE_C_BARE("service");
  ddog_Vec_Tag_PushResult push_result =
      ddog_Vec_Tag_push(&tags, service_key, to_slice_c_char(argv[1]));
  if (push_result.tag != DDOG_VEC_TAG_PUSH_RESULT_OK) {
    ddog_CharSlice message = ddog_Error_message(&push_result.err);
    fprintf(stderr, "Failed to push tag: %.*s\n", (int)message.len, message.ptr);
    ddog_Error_drop(&push_result.err);
    ddog_Vec_Tag_drop(tags);
    return 1;
  }

  auto endpoint =
      ddog_prof_Endpoint_agentless(DDOG_CHARSLICE_C_BARE("datad0g.com"), to_slice_c_char(api_key));
  auto exporter_result = ddog_prof_Exporter_new(DDOG_CHARSLICE_C_BARE("exporter-example"),
                                                DDOG_CHARSLICE_C_BARE("1.2.3"),
                                                DDOG_CHARSLICE_C_BARE("native"), &tags, endpoint);
  if (exporter_result.tag != DDOG_PROF_PROFILE_EXPORTER_RESULT_OK_HANDLE_PROFILE_EXPORTER) {
    ddog_CharSlice message = ddog_Error_message(&exporter_result.err);
    fprintf(stderr, "Failed to create exporter: %.*s\n", (int)message.len, message.ptr);
    ddog_Error_drop(&exporter_result.err);
    ddog_Vec_Tag_drop(tags);
    return 1;
  }
  ddog_prof_ProfileExporter exporter = exporter_result.ok;

  ddog_prof_Slice_Exporter_File files_to_compress = ddog_prof_Exporter_Slice_File_empty();
  ddog_prof_Slice_Exporter_File files_unmodified = ddog_prof_Exporter_Slice_File_empty();
  auto request_result = ddog_prof_Exporter_Request_build(
      &exporter, &encoded, files_to_compress, files_unmodified, nullptr, nullptr, nullptr);
  if (request_result.tag != DDOG_PROF_REQUEST_RESULT_OK_HANDLE_REQUEST) {
    ddog_CharSlice message = ddog_Error_message(&request_result.err);
    fprintf(stderr, "Failed to build request: %.*s\n", (int)message.len, message.ptr);
    ddog_Error_drop(&request_result.err);
    ddog_prof_Exporter_drop(&exporter);
    ddog_Vec_Tag_drop(tags);
    return 1;
  }

  ddog_CancellationToken cancel = ddog_CancellationToken_new();
  auto send_result = ddog_prof_Exporter_send(&exporter, &request_result.ok, &cancel);
  ddog_CancellationToken_drop(&cancel);
  if (send_result.tag != DDOG_PROF_RESULT_HTTP_STATUS_OK_HTTP_STATUS) {
    ddog_CharSlice message = ddog_Error_message(&send_result.err);
    fprintf(stderr, "Failed to send request: %.*s\n", (int)message.len, message.ptr);
    ddog_Error_drop(&send_result.err);
    ddog_prof_Exporter_drop(&exporter);
    ddog_Vec_Tag_drop(tags);
    return 1;
  }
  printf("Profile sent successfully (HTTP %d)\n", send_result.ok.code);

  // Cleanup
  ddog_prof_Exporter_drop(&exporter);
  ddog_Vec_Tag_drop(tags);
  ddog_prof_PprofBuilder_drop(&pprof);
  ddog_prof_Profile_drop(&profile);
  ddog_prof_ScratchPad_drop(&scratch);
  ddog_prof_ProfilesDictionary_drop(&dict);
  return 0;
}
