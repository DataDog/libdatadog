// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// Builds a profile through the v2 C API and lowers it to both a Datadog
// intake request and an OTLP request, checking each step.

#include <datadog/profiling_v2.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define CHECK(call)                                                                                                    \
    do {                                                                                                               \
        ddog_prof_Status status_ = (call);                                                                             \
        if (status_ != DDOG_PROF_STATUS_OK) {                                                                          \
            fprintf(stderr, "%s:%d: %s: %s\n", __FILE__, __LINE__, #call, ddog_prof_Status_message(status_));          \
            exit(1);                                                                                                   \
        }                                                                                                              \
    } while (0)

static ddog_prof_Str
str(const char* s)
{
    ddog_prof_Str out = { (const uint8_t*)s, strlen(s) };
    return out;
}

static uint32_t
intern(ddog_prof_StringTable* strings, const char* s)
{
    uint32_t id = 0;
    CHECK(ddog_prof_StringTable_intern(strings, str(s), &id));
    return id;
}

static void
print_request(const char* what, const ddog_prof_Request* request)
{
    ddog_prof_Str method = ddog_prof_Request_method(request);
    ddog_prof_Str path = ddog_prof_Request_path(request);
    printf("%s: %.*s %.*s, %zu body bytes\n",
           what,
           (int)method.len,
           (const char*)method.ptr,
           (int)path.len,
           (const char*)path.ptr,
           ddog_prof_Request_body(request).len);
    for (size_t i = 0; i < ddog_prof_Request_header_count(request); i++) {
        ddog_prof_Str name, value;
        CHECK(ddog_prof_Request_header(request, i, &name, &value));
        printf("  %.*s: %.*s\n", (int)name.len, (const char*)name.ptr, (int)value.len, (const char*)value.ptr);
    }
}

int
main(int argc, char** argv)
{
    const char* lock_dir = argc > 1 ? argv[1] : "/tmp";
    ddog_prof_StringTable* strings = NULL;
    ddog_prof_FunctionTable* functions = NULL;
    // Placeholders to fall back to when interning is busy later: reserved
    // while the tables are private, so creating them cannot be busy.
    ddog_prof_Str placeholder_strings[] = { str("<python function>"), str("<unknown file>") };
    uint32_t placeholder_ids[2] = { 0 };
    CHECK(ddog_prof_StringTable_new_with_reserved(
      4, 1 << 20, lock_dir, placeholder_strings, 2, placeholder_ids, 2, &strings));
    ddog_prof_Function placeholder_fn = { placeholder_ids[0], placeholder_ids[0], placeholder_ids[1], 0 };
    uint32_t placeholder_fn_id = 0;
    CHECK(ddog_prof_FunctionTable_new_with_reserved(
      4, lock_dir, &placeholder_fn, 1, &placeholder_fn_id, 1, &functions));
    uint32_t again = 0;
    CHECK(ddog_prof_StringTable_intern(strings, str("<python function>"), &again));
    if (placeholder_ids[0] == 0 || placeholder_ids[1] == 0 || placeholder_fn_id == 0 || again != placeholder_ids[0]) {
        fprintf(stderr, "reserved ids were not written\n");
        return 1;
    }
    // Mismatched lengths and bad pointers fail, leaving no table and zero ids.
    uint32_t zeroed[2] = { 7, 7 };
    ddog_prof_StringTable* never = NULL;
    if (ddog_prof_StringTable_new_with_reserved(4, 1 << 20, lock_dir, placeholder_strings, 1, zeroed, 2, &never) !=
          DDOG_PROF_STATUS_INVALID_ARGUMENT ||
        zeroed[0] != 0 || zeroed[1] != 0 || never != NULL) {
        fprintf(stderr, "mismatched lengths must fail and zero ids\n");
        return 1;
    }
    zeroed[0] = 7;
    if (ddog_prof_StringTable_new_with_reserved(4, 1 << 20, lock_dir, NULL, 1, zeroed, 1, &never) !=
          DDOG_PROF_STATUS_INVALID_ARGUMENT ||
        zeroed[0] != 0 || never != NULL) {
        fprintf(stderr, "a NULL reserved array must fail and zero ids\n");
        return 1;
    }

    ddog_prof_SampleType types[] = {
        { intern(strings, "cpu-time"), intern(strings, "nanoseconds") },
        { intern(strings, "wall-time"), intern(strings, "nanoseconds") },
        { intern(strings, "alloc-space"), intern(strings, "bytes") },
    };
    enum { CPU_TIME, WALL_TIME, ALLOC_SPACE }; // the types' ids: their positions
    ddog_prof_Period period = { str("wall-time"), str("nanoseconds"), 10000000 };
    ddog_prof_Profile* profile = NULL;
    CHECK(ddog_prof_Profile_new(strings, functions, types, 3, 1700000000000000000, &profile));

    ddog_prof_Function main_fn = { intern(strings, "main"), intern(strings, "main"), intern(strings, "app.py"), 1 };
    uint32_t main_id = 0;
    CHECK(ddog_prof_FunctionTable_intern(functions, &main_fn, &main_id));
    ddog_prof_Frame frames[] = { { 0, 10, main_id } };
    // Only what the sample measured; alloc-space is left out, so it is 0.
    ddog_prof_SampleValue values[] = { { CPU_TIME, 1000 }, { WALL_TIME, 2000 } };
    ddog_prof_Label labels[] = {
        { intern(strings, "thread name"), intern(strings, "MainThread"), { NULL, 0 }, 0, 0, false },
        { intern(strings, "task name"), 0, { (const uint8_t*)"task-1", 6 }, 0, 0, false },
    };
    ddog_prof_Sample sample = {
        frames, 1, values, 2, labels, 2, { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16 },
        { 0, 0, 0, 0, 0, 0, 0, 42 },
        7, 0,
    };
    for (int i = 0; i < 3; i++) {
        CHECK(ddog_prof_Profile_add(profile, &sample));
    }
    CHECK(ddog_prof_Profile_set_endpoint(profile, 7, str("GET /")));
    CHECK(ddog_prof_Profile_add_endpoint_count(profile, str("GET /"), 3));

    ddog_prof_Resource resource = { 0 };
    resource.service = str("demo");
    resource.env = str("dev");
    resource.language = str("c");
    resource.process_id = 1;

    ddog_prof_DatadogOptions options = { 0 };
    options.library_name = str("profiles_v2.c");
    options.library_version = str("2.0.0");
    options.family = str("native");
    options.period = &period;
    ddog_prof_Request* datadog = NULL;
    CHECK(ddog_prof_Profile_datadog_request(
      profile, 1700000060000000000, &resource, &options, NULL, &datadog)); // NULL: default encoding
    print_request("datadog", datadog);
    ddog_prof_Request_drop(datadog);

    ddog_prof_Scope scope = { str("profiles_v2.c"), str("2.0.0"), NULL, 0 };
    // OTLP carries only sanctioned sample types, mapped from this profile's.
    ddog_prof_OtlpProfile otlp_profiles[] = {
        { CPU_TIME, DDOG_PROF_OTLP_SAMPLE_TYPE_CPU_TIME, NULL },
        { ALLOC_SPACE, DDOG_PROF_OTLP_SAMPLE_TYPE_ALLOC_SIZE, NULL },
    };
    ddog_prof_EncodeOptions zstd = { 0, DDOG_PROF_COMPRESSION_ZSTD };
    ddog_prof_Request* otlp = NULL;
    CHECK(ddog_prof_Profile_otlp_request(profile,
                                         1700000060000000000,
                                         &resource,
                                         &scope,
                                         otlp_profiles,
                                         2,
                                         &zstd,
                                         &otlp));
    print_request("otlp", otlp);
    ddog_prof_Request_drop(otlp);

    // gzip, the compression every OTLP receiver must accept.
    ddog_prof_EncodeOptions gzip = { 0, DDOG_PROF_COMPRESSION_GZIP };
    CHECK(ddog_prof_Profile_otlp_request(
      profile, 1700000060000000000, &resource, &scope, otlp_profiles, 2, &gzip, &otlp));
    print_request("otlp gzip", otlp);
    ddog_prof_Bytes gzipped = ddog_prof_Request_body(otlp);
    if (gzipped.len < 18 || gzipped.ptr[0] != 0x1f || gzipped.ptr[1] != 0x8b) {
        fprintf(stderr, "expected a gzip body\n");
        return 1;
    }
    ddog_prof_Request_drop(otlp);

    ddog_prof_Request* pprof = NULL;
    CHECK(ddog_prof_Profile_encode_pprof(
      profile, 1700000060000000000, &period, &(ddog_prof_EncodeOptions){ 0, DDOG_PROF_COMPRESSION_NONE }, &pprof));
    printf("pprof: %zu bytes\n", ddog_prof_Request_body(pprof).len);
    ddog_prof_Request_drop(pprof);

    // Errors are statuses, not crashes.
    ddog_prof_Str bad = { (const uint8_t*)"\xff", 1 };
    uint32_t ignored;
    if (ddog_prof_StringTable_intern(strings, bad, &ignored) != DDOG_PROF_STATUS_INVALID_UTF8) {
        fprintf(stderr, "expected INVALID_UTF8\n");
        return 1;
    }
    if (ddog_prof_Profile_add(profile, NULL) != DDOG_PROF_STATUS_INVALID_ARGUMENT) {
        fprintf(stderr, "expected INVALID_ARGUMENT\n");
        return 1;
    }

    CHECK(ddog_prof_Profile_reset(profile, 1700000060000000000));
    ddog_prof_Profile_drop(profile);
    ddog_prof_FunctionTable_drop(functions);
    ddog_prof_StringTable_drop(strings);
    puts("ok");
    return 0;
}
