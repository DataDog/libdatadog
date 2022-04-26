extern "C" {
#include <ddprof/ffi.h>
}
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <thread>

static ddprof_ffi_Slice_c_char to_slice_c_char(const char *s) {
  return (ddprof_ffi_Slice_c_char){.ptr = s, .len = strlen(s)};
}

struct Deleter {
  void operator()(ddprof_ffi_Profile *object) {
    ddprof_ffi_Profile_free(object);
  }
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

  const ddprof_ffi_ValueType wall_time = {
      .type_ = DDPROF_FFI_CHARSLICE_C("wall-time"),
      .unit = DDPROF_FFI_CHARSLICE_C("nanoseconds"),
  };

  const ddprof_ffi_Slice_value_type sample_types = {&wall_time, 1};
  const ddprof_ffi_Period period = {wall_time, 60};
  std::unique_ptr<ddprof_ffi_Profile, Deleter> profile{
      ddprof_ffi_Profile_new(sample_types, &period)};

  ddprof_ffi_Line root_line = {
      .function =
          {
              .name = DDPROF_FFI_CHARSLICE_C("{main}"),
              .filename = DDPROF_FFI_CHARSLICE_C("/srv/example/index.php"),
          },
      .line = 0,
  };

  ddprof_ffi_Location root_location = {
      // yes, a zero-initialized mapping is valid
      .mapping = {},
      .lines = {&root_line, 1},
  };

  int64_t value = 10;
  const ddprof_ffi_Label label = {
      .key = DDPROF_FFI_CHARSLICE_C("language"),
      .str = DDPROF_FFI_CHARSLICE_C("php"),
  };
  ddprof_ffi_Sample sample = {
      .locations = {&root_location, 1},
      .values = {&value, 1},
      .labels = {&label, 1},
  };
  ddprof_ffi_Profile_add(profile.get(), sample);

  ddprof_ffi_SerializeResult serialize_result =
      ddprof_ffi_Profile_serialize(profile.get());
  if (serialize_result.tag == DDPROF_FFI_SERIALIZE_RESULT_ERR) {
    print_error("Failed to serialize profile: ", serialize_result.err);
    return 1;
  }

  ddprof_ffi_EncodedProfile *encoded_profile = &serialize_result.ok;

  ddprof_ffi_EndpointV3 endpoint = ddprof_ffi_EndpointV3_agentless(
      DDPROF_FFI_CHARSLICE_C("datad0g.com"), to_slice_c_char(api_key));

  ddprof_ffi_Vec_tag tags = ddprof_ffi_Vec_tag_new();
  ddprof_ffi_PushTagResult tag_result = ddprof_ffi_Vec_tag_push(
      &tags, DDPROF_FFI_CHARSLICE_C("service"), to_slice_c_char(service));
  if (tag_result.tag == DDPROF_FFI_PUSH_TAG_RESULT_ERR) {
    print_error("Failed to push tag: ", tag_result.err);
    ddprof_ffi_PushTagResult_drop(tag_result);
    return 1;
  }

  ddprof_ffi_PushTagResult_drop(tag_result);

  ddprof_ffi_NewProfileExporterV3Result exporter_new_result =
      ddprof_ffi_ProfileExporterV3_new(DDPROF_FFI_CHARSLICE_C("native"), &tags,
                                       endpoint);
  ddprof_ffi_Vec_tag_drop(tags);

  if (exporter_new_result.tag ==
      DDPROF_FFI_NEW_PROFILE_EXPORTER_V3_RESULT_ERR) {
    print_error("Failed to create exporter: ", exporter_new_result.err);
    ddprof_ffi_NewProfileExporterV3Result_drop(exporter_new_result);
    return 1;
  }

  auto exporter = exporter_new_result.ok;

  ddprof_ffi_File files_[] = {{
      .name = DDPROF_FFI_CHARSLICE_C("auto.pprof"),
      .file = ddprof_ffi_Vec_u8_as_slice(&encoded_profile->buffer),
  }};

  ddprof_ffi_Slice_file files = {.ptr = files_,
                                 .len = sizeof files_ / sizeof *files_};

  ddprof_ffi_Request *request = ddprof_ffi_ProfileExporterV3_build(
      exporter, encoded_profile->start, encoded_profile->end, files, nullptr,
      30000);

  ddprof_ffi_CancellationToken *cancel =
    ddprof_ffi_CancellationToken_new();
  ddprof_ffi_CancellationToken *cancel_for_background_thread =
    ddprof_ffi_CancellationToken_clone(cancel);

  // As an example of CancellationToken usage, here we create a background
  // thread that sleeps for some time and then cancels a request early (e.g.
  // before the timeout in ddprof_ffi_ProfileExporterV3_send is hit).
  //
  // If the request is faster than the sleep time, no cancellation takes place.
  std::thread trigger_cancel_if_request_takes_too_long_thread(
      [](ddprof_ffi_CancellationToken *cancel_for_background_thread) {
        int timeout_ms = 5000;
        std::this_thread::sleep_for(std::chrono::milliseconds(timeout_ms));
        printf("Request took longer than %d ms, triggering asynchronous "
               "cancellation\n",
               timeout_ms);
        ddprof_ffi_CancellationToken_cancel(cancel_for_background_thread);
        ddprof_ffi_CancellationToken_drop(cancel_for_background_thread);
      },
      cancel_for_background_thread);
  trigger_cancel_if_request_takes_too_long_thread.detach();

  int exit_code = 0;
  ddprof_ffi_SendResult send_result =
      ddprof_ffi_ProfileExporterV3_send(exporter, request, cancel);
  if (send_result.tag == DDPROF_FFI_SEND_RESULT_ERR) {
    print_error("Failed to send profile: ", send_result.err);
    exit_code = 1;
  } else {
    printf("Response code: %d\n", send_result.http_response.code);
  }

  ddprof_ffi_NewProfileExporterV3Result_drop(exporter_new_result);
  ddprof_ffi_SendResult_drop(send_result);
  ddprof_ffi_CancellationToken_drop(cancel);
  return exit_code;
}
