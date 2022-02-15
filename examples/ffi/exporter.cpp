extern "C"
{
#include <ddprof/ffi.h>
}
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>

static ddprof_ffi_ByteSlice to_byteslice(const char *s)
{
    return {.ptr = (uint8_t *)s,
            .len = strlen(s)};
}

static ddprof_ffi_Slice_c_char to_slice_c_char(const char *s)
{
    return {.ptr = s,
            .len = strlen(s)};
}

struct Deleter
{
    void operator()(ddprof_ffi_Profile *object)
    {
        ddprof_ffi_Profile_free(object);
    }
    void operator()(ddprof_ffi_ProfileExporterV3 *object)
    {
        ddprof_ffi_ProfileExporterV3_delete(object);
    }
    void operator()(ddprof_ffi_EncodedProfile *object)
    {
        ddprof_ffi_EncodedProfile_delete(object);
    }
};

template <typename T>
class Holder
{
public:
    explicit Holder(T *object) : _object(object) {}
    ~Holder() { Deleter{}(_object); }
    Holder(const Holder &) = delete;
    Holder &operator=(const Holder &) = delete;

    operator T *() { return _object; }
    T *operator->() { return _object; }

    T *_object;
};

template <typename T>
void print_error(const char *s, const T &err)
{
    printf("%s: (%.*s)\n", s, static_cast<int>(err.len), err.ptr);
}

int main(int argc, char* argv[])
{
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
        .type_ = to_slice_c_char("wall-time"),
        .unit = to_slice_c_char("nanoseconds"),
    };

    const ddprof_ffi_Slice_value_type sample_types = {&wall_time, 1};
    const ddprof_ffi_Period period = {wall_time, 60};
    Holder<ddprof_ffi_Profile> profile{ddprof_ffi_Profile_new(sample_types, &period)};

    ddprof_ffi_Line root_line = {
        .function = {
            .name = to_slice_c_char("{main}"),
            .filename = to_slice_c_char("/srv/example/index.php")},
        .line = 0,
    };

    ddprof_ffi_Location root_location = {
        // yes, a zero-initialized mapping is valid
        .mapping = {},
        .lines = {&root_line, 1},
    };

    int64_t value = 10;
    const ddprof_ffi_Label label = {
        .key = to_slice_c_char("language"),
        .str = to_slice_c_char("php"),
    };
    ddprof_ffi_Sample sample = {
        .locations = {&root_location, 1},
        .values = {&value, 1},
        .labels = {&label, 1},
    };
    ddprof_ffi_Profile_add(profile, sample);

    Holder<ddprof_ffi_EncodedProfile> encoded_profile{ddprof_ffi_Profile_serialize(profile)};

    ddprof_ffi_EndpointV3 endpoint = ddprof_ffi_EndpointV3_agentless(to_byteslice("datad0g.com"), to_byteslice(api_key));
    ddprof_ffi_Tag tags[] = {{to_byteslice("service"), to_byteslice(service)}};
    ddprof_ffi_NewProfileExporterV3Result exporter_new_result =
        ddprof_ffi_ProfileExporterV3_new(
            to_byteslice("native"), ddprof_ffi_Slice_tag{.ptr = tags, .len = sizeof(tags) / sizeof(tags[0])}, endpoint);

    if (exporter_new_result.tag == DDPROF_FFI_NEW_PROFILE_EXPORTER_V3_RESULT_ERR)
    {
        print_error("Failed to create exporter: ", exporter_new_result.err);
        return 1;
    }

    Holder<ddprof_ffi_ProfileExporterV3> exporter{exporter_new_result.ok};

    ddprof_ffi_Buffer profile_buffer = {
        .ptr = encoded_profile->buffer.ptr,
        .len = encoded_profile->buffer.len,
        .capacity = encoded_profile->buffer.capacity,
    };

    ddprof_ffi_File files_[] = {{
        .name = to_byteslice("auto.pprof"),
        .file = &profile_buffer,
    }};

    ddprof_ffi_Slice_file files = {
        .ptr = files_, .len = sizeof files_ / sizeof *files_};

    ddprof_ffi_Request *request = ddprof_ffi_ProfileExporterV3_build(
        exporter, encoded_profile->start, encoded_profile->end, files, 10000);

    ddprof_ffi_SendResult send_result = ddprof_ffi_ProfileExporterV3_send(exporter, request);
    if (send_result.tag == DDPROF_FFI_SEND_RESULT_FAILURE)
    {
        print_error("Failed to send profile: ", send_result.failure);
        return 1;
    }
    else
    {
        printf("Response code: %d\n", send_result.http_response.code);
    }
}