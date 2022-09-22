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

static ddog_Slice_c_char to_slice_c_char(const char *s) { return {.ptr = s, .len = strlen(s)}; }

struct Deleter {
  void operator()(ddog_Profile *object) { ddog_Profile_free(object); }
};

template <typename T> void print_error(const char *s, const T &err) {
  printf("%s (%.*s)\n", s, static_cast<int>(err.len), err.ptr);
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

  const ddog_ValueType wall_time = {
      .type_ = DDOG_CHARSLICE_C("wall-time"),
      .unit = DDOG_CHARSLICE_C("nanoseconds"),
  };

  const ddog_Slice_value_type sample_types = {&wall_time, 1};
  const ddog_Period period = {wall_time, 60};
  std::unique_ptr<ddog_Profile, Deleter> profile{ddog_Profile_new(sample_types, &period, nullptr)};

  ddog_Line root_line = {
      .function =
          {
              .name = DDOG_CHARSLICE_C("{main}"),
              .filename = DDOG_CHARSLICE_C("/srv/example/index.php"),
          },
      .line = 0,
  };

  ddog_Location root_location = {
      // yes, a zero-initialized mapping is valid
      .mapping = {},
      .lines = {&root_line, 1},
  };

  int64_t value = 10;
  const ddog_Label label = {
      .key = DDOG_CHARSLICE_C("language"),
      .str = DDOG_CHARSLICE_C("php"),
  };
  ddog_Sample sample = {
      .locations = {&root_location, 1},
      .values = {&value, 1},
      .labels = {&label, 1},
  };
  ddog_Profile_add(profile.get(), sample);

  ddog_SerializeResult serialize_result = ddog_Profile_serialize(profile.get(), nullptr, nullptr);
  if (serialize_result.tag == DDOG_SERIALIZE_RESULT_ERR) {
    print_error("Failed to serialize profile: ", serialize_result.err);
    return 1;
  }

  ddog_EncodedProfile *encoded_profile = &serialize_result.ok;

  ddog_Endpoint endpoint =
      ddog_Endpoint_agentless(DDOG_CHARSLICE_C("datad0g.com"), to_slice_c_char(api_key));

  ddog_Vec_tag tags = ddog_Vec_tag_new();
  ddog_PushTagResult tag_result =
      ddog_Vec_tag_push(&tags, DDOG_CHARSLICE_C("service"), to_slice_c_char(service));
  if (tag_result.tag == DDOG_PUSH_TAG_RESULT_ERR) {
    print_error("Failed to push tag: ", tag_result.err);
    ddog_PushTagResult_drop(tag_result);
    return 1;
  }

  ddog_PushTagResult_drop(tag_result);

  ddog_NewProfileExporterResult exporter_new_result =
      ddog_ProfileExporter_new(DDOG_CHARSLICE_C("native"), &tags, endpoint);
  ddog_Vec_tag_drop(tags);

  if (exporter_new_result.tag == DDOG_NEW_PROFILE_EXPORTER_RESULT_ERR) {
    print_error("Failed to create exporter: ", exporter_new_result.err);
    ddog_NewProfileExporterResult_drop(exporter_new_result);
    return 1;
  }

  auto exporter = exporter_new_result.ok;

  ddog_File files_[] = {{
      .name = DDOG_CHARSLICE_C("auto.pprof"),
      .file = ddog_Vec_u8_as_slice(&encoded_profile->buffer),
  }};

  ddog_Slice_file files = {.ptr = files_, .len = sizeof files_ / sizeof *files_};

  ddog_Request *request = ddog_ProfileExporter_build(
    exporter,
    encoded_profile->start,
    encoded_profile->end,
    files,
    nullptr,
    30000,
    DDOG_CHARSLICE_C("exporter-example"),
    DDOG_CHARSLICE_C("1.2.3")
  );

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
  ddog_SendResult send_result = ddog_ProfileExporter_send(exporter, request, cancel);
  if (send_result.tag == DDOG_SEND_RESULT_ERR) {
    print_error("Failed to send profile: ", send_result.err);
    exit_code = 1;
  } else {
    printf("Response code: %d\n", send_result.http_response.code);
  }

  ddog_NewProfileExporterResult_drop(exporter_new_result);
  ddog_SendResult_drop(send_result);
  ddog_CancellationToken_drop(cancel);
  return exit_code;
}
