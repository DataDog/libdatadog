extern "C" {
  #include <datadog/common.h>
  #include <datadog/profiling.h>
}
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
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
      .type_ = DDOG_CHARSLICE_C("wall-time"),
      .unit = DDOG_CHARSLICE_C("nanoseconds"),
  };

  const ddog_prof_Slice_ValueType sample_types = {&wall_time, 1};
  const ddog_prof_Period period = {wall_time, 60};
  std::unique_ptr<ddog_prof_Profile, Deleter> profile{ ddog_prof_Profile_new(sample_types, &period, nullptr) };

  ddog_prof_Line root_line = {
      .function =
          {
              .name = DDOG_CHARSLICE_C("{main}"),
              .filename = DDOG_CHARSLICE_C("/srv/example/index.php"),
          },
      .line = 0,
  };

  ddog_prof_Location root_location = {
      // yes, a zero-initialized mapping is valid
      .mapping = {},
      .lines = {&root_line, 1},
  };

  int64_t value = 10;
  const ddog_prof_Label label = {
      .key = DDOG_CHARSLICE_C("language"),
      .str = DDOG_CHARSLICE_C("php"),
  };
  ddog_prof_Sample sample = {
      .locations = {&root_location, 1},
      .values = {&value, 1},
      .labels = {&label, 1},
  };
  auto add_result = ddog_prof_Profile_add(profile.get(), sample);
  if (add_result.tag != DDOG_PROF_PROFILE_ADD_RESULT_OK) {
    print_error("Failed to add sample to profile: ", add_result.err);
    ddog_Error_drop(&add_result.err);
    return 1;
  }

  ddog_prof_Profile_SerializeResult serialize_result = ddog_prof_Profile_serialize(profile.get(), nullptr, nullptr);
  if (serialize_result.tag == DDOG_PROF_PROFILE_SERIALIZE_RESULT_ERR) {
    print_error("Failed to serialize profile: ", serialize_result.err);
    ddog_Error_drop(&serialize_result.err);
    return 1;
  }

  ddog_prof_EncodedProfile *encoded_profile = &serialize_result.ok;

  ddog_Endpoint endpoint =
      ddog_Endpoint_agentless(DDOG_CHARSLICE_C("datad0g.com"), to_slice_c_char(api_key));

  ddog_Vec_Tag tags = ddog_Vec_Tag_new();
  ddog_Vec_Tag_PushResult tag_result =
      ddog_Vec_Tag_push(&tags, DDOG_CHARSLICE_C("service"), to_slice_c_char(service));
  if (tag_result.tag == DDOG_VEC_TAG_PUSH_RESULT_ERR) {
    print_error("Failed to push tag: ", tag_result.err);
    ddog_Error_drop(&tag_result.err);
    return 1;
  }

  ddog_prof_Exporter_NewResult exporter_new_result = ddog_prof_Exporter_new(
      DDOG_CHARSLICE_C("exporter-example"),
      DDOG_CHARSLICE_C("1.2.3"),
      DDOG_CHARSLICE_C("native"),
      &tags,
      endpoint
  );
  ddog_Vec_Tag_drop(tags);

  if (exporter_new_result.tag == DDOG_PROF_EXPORTER_NEW_RESULT_ERR) {
    print_error("Failed to create exporter: ", exporter_new_result.err);
    ddog_Error_drop(&exporter_new_result.err);
    return 1;
  }

  auto exporter = exporter_new_result.ok;

  ddog_prof_Exporter_File files_[] = {{
      .name = DDOG_CHARSLICE_C("auto.pprof"),
      .file = ddog_Vec_U8_as_slice(&encoded_profile->buffer),
  }};

  ddog_prof_Exporter_Slice_File files = {.ptr = files_, .len = sizeof files_ / sizeof *files_};

  ddog_prof_Exporter_Request_BuildResult build_result = ddog_prof_Exporter_Request_build(
    exporter,
    encoded_profile->start,
    encoded_profile->end,
    files,
    nullptr,
    nullptr,
    30000
  );
  ddog_prof_EncodedProfile_drop(encoded_profile);

  if (build_result.tag == DDOG_PROF_EXPORTER_REQUEST_BUILD_RESULT_ERR) {
    print_error("Failed to build request: ", build_result.err);
    ddog_Error_drop(&build_result.err);
    return 1;
  }

  auto &request = build_result.ok;

  ddog_CancellationToken *cancel = ddog_CancellationToken_new();
  ddog_CancellationToken *cancel_for_background_thread = ddog_CancellationToken_clone(cancel);

  // As an example of CancellationToken usage, here we create a background
  // thread that sleeps for some time and then cancels a request early (e.g.
  // before the timeout in ddog_ProfileExporter_send is hit).
  //
  // If the request is faster than the sleep time, no cancellation takes place.
  std::thread trigger_cancel_if_request_takes_too_long_thread(
      [](ddog_CancellationToken *cancel_for_background_thread) {
        int timeout_ms = 5000;
        std::this_thread::sleep_for(std::chrono::milliseconds(timeout_ms));
        printf("Request took longer than %d ms, triggering asynchronous "
               "cancellation\n",
               timeout_ms);
        ddog_CancellationToken_cancel(cancel_for_background_thread);
        ddog_CancellationToken_drop(cancel_for_background_thread);
      },
      cancel_for_background_thread);
  trigger_cancel_if_request_takes_too_long_thread.detach();

  int exit_code = 0;
  ddog_prof_Exporter_SendResult send_result = ddog_prof_Exporter_send(exporter, &request, cancel);
  if (send_result.tag == DDOG_PROF_EXPORTER_SEND_RESULT_ERR) {
    print_error("Failed to send profile: ", send_result.err);
    exit_code = 1;
    ddog_Error_drop(&send_result.err);
  } else {
    printf("Response code: %d\n", send_result.http_response.code);
  }

  ddog_prof_Exporter_Request_drop(&request);

  ddog_prof_Exporter_drop(exporter);
  ddog_CancellationToken_drop(cancel);
  return exit_code;
}
