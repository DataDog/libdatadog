#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <datadog/common.h>
#include <datadog/profiling.h>
#include <memory>
#include <thread>

static ddog_CharSlice to_slice_c_char(const char *s) { return {.ptr = s, .len = strlen(s)}; }

struct Deleter {
  void operator()(ddog_prof_Profile *object) { ddog_prof_Profile_drop(object); }
};

void print_error(const char *s, const ddog_Error &err) {
  auto charslice = ddog_Error_message(&err);
  printf("%s (%.*s)\n", s, static_cast<int>(charslice.len), charslice.ptr);
}

int main(int argc, char *argv[]) {
  if (argc != 2) {
    printf("Usage: exporter SERVICE_NAME\n");
    return 1;
  }
  const char *api_key = getenv("DD_API_KEY");
  if (!api_key) {
    printf("DD_API_KEY environment variable is no set\n");
    return 1;
  }

  const auto service = argv[1];

  const ddog_prof_ValueType wall_time = {
      .type_ = DDOG_CHARSLICE_C_BARE("wall-time"),
      .unit = DDOG_CHARSLICE_C_BARE("nanoseconds"),
  };

  const ddog_prof_Slice_ValueType sample_types = {&wall_time, 1};
  const ddog_prof_Period period = {wall_time, 60};
  ddog_prof_Profile_NewResult profile_new_result = ddog_prof_Profile_new(sample_types, &period);
  if (profile_new_result.tag != DDOG_PROF_PROFILE_NEW_RESULT_OK) {
    print_error("Failed to make new profile: ", profile_new_result.err);
    ddog_Error_drop(&profile_new_result.err);
    exit(EXIT_FAILURE);
  }
  std::unique_ptr<ddog_prof_Profile, Deleter> profile{&profile_new_result.ok};

  ddog_prof_Location root_location = {
      // yes, a zero-initialized mapping is valid
      .mapping = {},
      .function =
          {
              .name = DDOG_CHARSLICE_C_BARE("{main}"),
              .filename = DDOG_CHARSLICE_C_BARE("/srv/example/index.php"),
          },
  };

  int64_t value = 10;
  const ddog_prof_Label label = {
      .key = DDOG_CHARSLICE_C_BARE("language"),
      .str = DDOG_CHARSLICE_C_BARE("php"),
  };
  ddog_prof_Sample sample = {
      .locations = {&root_location, 1},
      .values = {&value, 1},
      .labels = {&label, 1},
  };
  auto add_result = ddog_prof_Profile_add(profile.get(), sample, 0);
  if (add_result.tag != DDOG_PROF_PROFILE_RESULT_OK) {
    print_error("Failed to add sample to profile: ", add_result.err);
    ddog_Error_drop(&add_result.err);
    return 1;
  }

  uintptr_t offset[1] = {0};
  ddog_prof_Slice_Usize offsets_slice = {.ptr = offset, .len = 1};
  ddog_CharSlice empty_charslice = DDOG_CHARSLICE_C_BARE("");

  auto upscaling_addresult = ddog_prof_Profile_add_upscaling_rule_proportional(
      profile.get(), offsets_slice, empty_charslice, empty_charslice, 1, 1);

  if (upscaling_addresult.tag == DDOG_PROF_PROFILE_RESULT_ERR) {
    print_error("Failed to add an upscaling rule: ", upscaling_addresult.err);
    ddog_Error_drop(&upscaling_addresult.err);
    // in this specific case, we want to fail the execution. But in general, we should not
    return 1;
  }

  ddog_prof_Profile_SerializeResult serialize_result =
      ddog_prof_Profile_serialize(profile.get(), nullptr, nullptr);
  if (serialize_result.tag == DDOG_PROF_PROFILE_SERIALIZE_RESULT_ERR) {
    print_error("Failed to serialize profile: ", serialize_result.err);
    ddog_Error_drop(&serialize_result.err);
    return 1;
  }

  auto *encoded_profile = &serialize_result.ok;

  auto endpoint =
      ddog_prof_Endpoint_agentless(DDOG_CHARSLICE_C_BARE("datad0g.com"), to_slice_c_char(api_key));

  ddog_Vec_Tag tags = ddog_Vec_Tag_new();
  ddog_Vec_Tag_PushResult tag_result =
      ddog_Vec_Tag_push(&tags, DDOG_CHARSLICE_C_BARE("service"), to_slice_c_char(service));
  if (tag_result.tag == DDOG_VEC_TAG_PUSH_RESULT_ERR) {
    print_error("Failed to push tag: ", tag_result.err);
    ddog_Error_drop(&tag_result.err);
    return 1;
  }

  auto exporter_new_result = ddog_prof_Exporter_new(
      DDOG_CHARSLICE_C_BARE("exporter-example"), DDOG_CHARSLICE_C_BARE("1.2.3"),
      DDOG_CHARSLICE_C_BARE("native"), &tags, endpoint);
  ddog_Vec_Tag_drop(tags);

  if (exporter_new_result.tag == DDOG_PROF_PROFILE_EXPORTER_RESULT_ERR_HANDLE_PROFILE_EXPORTER) {
    print_error("Failed to create exporter: ", exporter_new_result.err);
    ddog_Error_drop(&exporter_new_result.err);
    return 1;
  }

  auto exporter = &exporter_new_result.ok;

  auto files_to_compress_and_export = ddog_prof_Exporter_Slice_File_empty();
  auto files_to_export_unmodified = ddog_prof_Exporter_Slice_File_empty();

  ddog_CharSlice internal_metadata_example = DDOG_CHARSLICE_C_BARE(
      "{\"no_signals_workaround_enabled\": \"true\", \"execution_trace_enabled\": \"false\"}");

  ddog_CharSlice info_example =
      DDOG_CHARSLICE_C_BARE("{\"application\": {\"start_time\": \"2024-01-24T11:17:22+0000\"}, "
                            "\"platform\": {\"kernel\": \"Darwin Kernel 22.5.0\"}}");

  auto res = ddog_prof_Exporter_set_timeout(exporter, 30000);
  if (res.tag == DDOG_VOID_RESULT_ERR) {
    print_error("Failed to set the timeout", res.err);
    ddog_Error_drop(&res.err);
    return 1;
  }

  auto build_result = ddog_prof_Exporter_Request_build(
      exporter, encoded_profile, files_to_compress_and_export, files_to_export_unmodified, nullptr, nullptr,
      &internal_metadata_example, &info_example);
  ddog_prof_EncodedProfile_drop(encoded_profile);

  if (build_result.tag == DDOG_PROF_REQUEST_RESULT_ERR_HANDLE_REQUEST) {
    print_error("Failed to build request: ", build_result.err);
    ddog_Error_drop(&build_result.err);
    return 1;
  }

  auto request = &build_result.ok;

  auto cancel = ddog_CancellationToken_new();
  auto cancel_for_background_thread = ddog_CancellationToken_clone(&cancel);

  // As an example of CancellationToken usage, here we create a background
  // thread that sleeps for some time and then cancels a request early (e.g.
  // before the timeout in ddog_ProfileExporter_send is hit).
  //
  // If the request is faster than the sleep time, no cancellation takes place.
  std::thread trigger_cancel_if_request_takes_too_long_thread(
      [](ddog_CancellationToken cancel_for_background_thread) {
        int timeout_ms = 5000;
        std::this_thread::sleep_for(std::chrono::milliseconds(timeout_ms));
        printf("Request took longer than %d ms, triggering asynchronous "
               "cancellation\n",
               timeout_ms);
        ddog_CancellationToken_cancel(&cancel_for_background_thread);
        ddog_CancellationToken_drop(&cancel_for_background_thread);
      },
      cancel_for_background_thread);
  trigger_cancel_if_request_takes_too_long_thread.detach();

  int exit_code = 0;
  auto send_result = ddog_prof_Exporter_send(exporter, request, &cancel);
  if (send_result.tag == DDOG_PROF_RESULT_HTTP_STATUS_ERR_HTTP_STATUS) {
    print_error("Failed to send profile: ", send_result.err);
    exit_code = 1;
    ddog_Error_drop(&send_result.err);
  } else {
    printf("Response code: %d\n", send_result.ok.code);
  }

  ddog_prof_Exporter_Request_drop(request);
  ddog_prof_Exporter_drop(exporter);
  ddog_CancellationToken_drop(&cancel);
  return exit_code;
}
