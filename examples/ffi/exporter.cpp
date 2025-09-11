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
    exit(EXIT_FAILURE);
  }
}

int main(int argc, char **argv) {
  if (argc != 2) {
    printf("Usage: %s <service_name>\n", argv[0]);
    return 1;
  }

  // Core handles
  ddog_prof_ProfilesDictionaryHandle dict = NULL;
  check_ok(ddog_prof_ProfilesDictionary_new(&dict), "ProfilesDictionary_new");

  ddog_prof_ScratchPadHandle scratch = NULL;
  check_ok(ddog_prof_ScratchPad_new(&scratch), "ScratchPad_new");

  // ValueType: wall-time / nanoseconds
  ddog_prof_StringId vt_type = {0}, vt_unit = {0};
  const ddog_CharSlice wall_time = DDOG_CHARSLICE_C_BARE("wall-time");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&vt_type, dict, wall_time,
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(type)");
  const ddog_CharSlice nanoseconds = DDOG_CHARSLICE_C_BARE("nanoseconds");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&vt_unit, dict, nanoseconds,
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(unit)");
  ddog_prof_ValueType vt = {.type_id = vt_type, .unit_id = vt_unit};

  // Insert a function and mapping in the dictionary
  ddog_prof_StringId fn_name = {0}, fn_sys = {0}, fn_file = {0};
  const ddog_CharSlice root_str = DDOG_CHARSLICE_C_BARE("root");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&fn_name, dict, root_str,
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(fn name)");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&fn_sys, dict, root_str,
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(fn system)");
  const ddog_CharSlice root_cpp = DDOG_CHARSLICE_C_BARE("root.cpp");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&fn_file, dict, root_cpp,
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(fn file)");
  ddog_prof_Function func = {.name = fn_name, .system_name = fn_sys, .file_name = fn_file};
  ddog_prof_FunctionId func_id = NULL;
  check_ok(ddog_prof_ProfilesDictionary_insert_function(&func_id, dict, &func), "insert_function");

  ddog_prof_StringId map_file = {0}, map_build = {0};
  const ddog_CharSlice bin_example = DDOG_CHARSLICE_C_BARE("/bin/example");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&map_file, dict, bin_example,
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(map filename)");
  const ddog_CharSlice deadbeef = DDOG_CHARSLICE_C_BARE("deadbeef");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&map_build, dict, deadbeef,
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(map build)");
  ddog_prof_Mapping mapping = {.memory_start = 0,
                               .memory_limit = 0,
                               .file_offset = 0,
                               .filename = map_file,
                               .build_id = map_build};
  ddog_prof_MappingId map_id = NULL;
  check_ok(ddog_prof_ProfilesDictionary_insert_mapping(&map_id, dict, &mapping), "insert_mapping");

  // Insert a location and stack in the scratchpad
  ddog_prof_Line line = {.line_number = 42, .function_id = func_id};
  ddog_prof_Location loc = {.address = 0, .mapping_id = map_id, .line = line};
  ddog_prof_LocationId loc_id = NULL;
  check_ok(ddog_prof_ScratchPad_insert_location(&loc_id, scratch, &loc),
           "ScratchPad_insert_location");
  ddog_prof_LocationId locs[1] = {loc_id};
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

  // Build one sample via SampleBuilder
  ddog_prof_SampleBuilderHandle sb = NULL;
  check_ok(ddog_prof_SampleBuilder_new(&sb, scratch), "SampleBuilder_new");
  check_ok(ddog_prof_SampleBuilder_stack_id(sb, stack_id), "SampleBuilder_stack_id");
  check_ok(ddog_prof_SampleBuilder_value(sb, 10'000'000LL), "SampleBuilder_value");
  check_ok(ddog_prof_SampleBuilder_build_into_profile(&sb, profile),
           "SampleBuilder_build_into_profile");

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

  const ddog_CharSlice localhost_url = DDOG_CHARSLICE_C_BARE("http://localhost:8126");
  ddog_prof_Endpoint endpoint = ddog_prof_Endpoint_agent(localhost_url);
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
