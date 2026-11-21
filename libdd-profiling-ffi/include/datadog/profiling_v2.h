// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// C API of libdd-profiling v2: profiles lowered to pprof (Datadog intake) or
// OTLP profiles, as HTTP requests for the caller's own client to send.
//
// - Tables are counted handles: _clone adds one, _drop removes one.
// - A profile is not locked: serialize all calls on one profile.
// - Every function returns a ddog_prof_Status; DDOG_PROF_STATUS_OK is 0.

#ifndef DDOG_PROFILING_V2_H
#define DDOG_PROFILING_V2_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum ddog_prof_Status
{
    DDOG_PROF_STATUS_OK = 0,
    DDOG_PROF_STATUS_INVALID_ARGUMENT,
    DDOG_PROF_STATUS_INVALID_UTF8,
    DDOG_PROF_STATUS_OUT_OF_MEMORY,
    DDOG_PROF_STATUS_BUSY,
    DDOG_PROF_STATUS_FULL,
    DDOG_PROF_STATUS_ARENA_FULL,
    DDOG_PROF_STATUS_TOO_LONG,
    DDOG_PROF_STATUS_LOCK_FAILED,
    DDOG_PROF_STATUS_CORRUPT,
    DDOG_PROF_STATUS_MAPPING_FAILED,
    DDOG_PROF_STATUS_UNSUPPORTED_PAGE_SIZE,
    DDOG_PROF_STATUS_TOO_LARGE,
    DDOG_PROF_STATUS_NOTHING_TO_ENCODE,
    DDOG_PROF_STATUS_TOO_MANY,
    DDOG_PROF_STATUS_TOO_MANY_HANDLES,
    DDOG_PROF_STATUS_COMPRESSION,
    DDOG_PROF_STATUS_COMPRESSION_UNAVAILABLE,
} ddog_prof_Status;

// How a request's profile is compressed. AUTO is the implementation's choice
// per format: for pprof (the Datadog intake, which the Agent forwards as-is),
// zstd when built in, else gzip when built in, else none; for OTLP none (a
// collector decompresses and processes the profile, and its exporters choose
// their own compression). GZIP needs the `gzip` feature (which `otel` enables)
// and ZSTD the `zstd` feature; otherwise the request fails with
// DDOG_PROF_STATUS_COMPRESSION_UNAVAILABLE. OTLP requests set Content-Encoding.
typedef enum ddog_prof_Compression
{
    DDOG_PROF_COMPRESSION_AUTO,
    DDOG_PROF_COMPRESSION_NONE,
    DDOG_PROF_COMPRESSION_GZIP,
    DDOG_PROF_COMPRESSION_ZSTD,
} ddog_prof_Compression;

// The sample types OTLP requests may carry, each with a fixed name and the
// unit its source sample type must have.
typedef enum ddog_prof_OtlpSampleType
{
    DDOG_PROF_OTLP_SAMPLE_TYPE_CPU_TIME,          // "cpu-time", nanoseconds
    DDOG_PROF_OTLP_SAMPLE_TYPE_ALLOC_SIZE,        // "alloc-size", bytes
    DDOG_PROF_OTLP_SAMPLE_TYPE_ALLOC_SAMPLES,     // "alloc-samples", count
    DDOG_PROF_OTLP_SAMPLE_TYPE_HEAP_LIVE_SIZE,    // "heap-live-size", bytes
    DDOG_PROF_OTLP_SAMPLE_TYPE_HEAP_LIVE_SAMPLES, // "heap-live-samples", count
} ddog_prof_OtlpSampleType;

// UTF-8, not necessarily NUL-terminated. ptr may be NULL when len is 0.
typedef struct ddog_prof_Str
{
    const uint8_t* ptr;
    size_t len;
} ddog_prof_Str;

typedef struct ddog_prof_Bytes
{
    const uint8_t* ptr;
    size_t len;
} ddog_prof_Bytes;

typedef struct ddog_prof_StringTable ddog_prof_StringTable;
typedef struct ddog_prof_FunctionTable ddog_prof_FunctionTable;
typedef struct ddog_prof_Profile ddog_prof_Profile;
typedef struct ddog_prof_Request ddog_prof_Request;

// Fields are string ids.
typedef struct ddog_prof_Function
{
    uint32_t name;
    uint32_t system_name;
    uint32_t filename;
    uint32_t start_line;
} ddog_prof_Function;

typedef struct ddog_prof_Frame
{
    uint64_t address;
    int64_t line;
    uint32_t function; // a function id
} ddog_prof_Frame;

// A sample type's id is its position in the types the profile was created
// with.
typedef uint32_t ddog_prof_SampleTypeId;

typedef struct ddog_prof_SampleType
{
    uint32_t name; // string id, such as "cpu-time"
    uint32_t unit; // string id, such as "nanoseconds"
} ddog_prof_SampleType;

// A sample's value for one sample type.
typedef struct ddog_prof_SampleValue
{
    ddog_prof_SampleTypeId sample_type;
    int64_t value;
} ddog_prof_SampleValue;

// A sampling period as a format records it: what is counted between samples
// (such as "wall-time" in "nanoseconds") and how much. It affects no value.
typedef struct ddog_prof_Period
{
    ddog_prof_Str name;
    ddog_prof_Str unit;
    int64_t value;
} ddog_prof_Period;

// A string label or a number with an optional unit (num_unit a string id, 0
// for none). A string label uses str_value when it is non-empty: the profile
// keeps that string until its next reset, which suits high-cardinality
// values (task names, exception messages). Otherwise str is a string id.
typedef struct ddog_prof_Label
{
    uint32_t key;
    uint32_t str;
    ddog_prof_Str str_value;
    int64_t num;
    uint32_t num_unit;
    bool is_num;
} ddog_prof_Label;

// values holds what the sample measured, each sample type at most once; types
// left out count as 0. frames are leaf first. All-zero
// trace and span ids mean no span. timestamp_ns 0 aggregates the sample.
// pprof keeps the span id and local root span id (as labels); OTLP keeps
// the trace id and span id (as a link). Each drops what it cannot carry.
typedef struct ddog_prof_Sample
{
    const ddog_prof_Frame* frames;
    size_t frames_len;
    const ddog_prof_SampleValue* values;
    size_t values_len;
    const ddog_prof_Label* labels;
    size_t labels_len;
    uint8_t trace_id[16];
    uint8_t span_id[8];
    uint64_t local_root_span_id;
    int64_t timestamp_ns;
} ddog_prof_Sample;

typedef struct ddog_prof_Tag
{
    ddog_prof_Str key;
    ddog_prof_Str value;
} ddog_prof_Tag;

// Empty strings and a zero process id are omitted. Mapped to Datadog tags
// for pprof and to semantic-convention resource attributes for OTLP.
typedef struct ddog_prof_Resource
{
    ddog_prof_Str service;
    ddog_prof_Str env;
    ddog_prof_Str version;
    ddog_prof_Str host_name;
    ddog_prof_Str runtime_id;
    ddog_prof_Str language;
    ddog_prof_Str runtime_name;
    ddog_prof_Str runtime_version;
    ddog_prof_Str runtime_description; // OTLP only: process.runtime.description
    ddog_prof_Str profiler_version;
    ddog_prof_Str container_id;
    uint64_t cgroup_inode; // OTLP only: datadog.container.cgroup_inode (an int); 0 for none
    uint32_t process_id;
    const ddog_prof_Tag* tags;
    size_t tags_len;
} ddog_prof_Resource;

typedef struct ddog_prof_Attachment
{
    ddog_prof_Str name;
    ddog_prof_Bytes bytes;
} ddog_prof_Attachment;

typedef struct ddog_prof_DatadogOptions
{
    ddog_prof_Str library_name;    // DD-EVP-ORIGIN
    ddog_prof_Str library_version; // DD-EVP-ORIGIN-VERSION
    ddog_prof_Str family;          // such as "python"
    ddog_prof_Str internal_json;   // a JSON object, or empty
    ddog_prof_Str info_json;       // a JSON object, or empty
    ddog_prof_Str process_tags;
    const ddog_prof_Attachment* attachments;
    size_t attachments_len;
    const ddog_prof_Period* period; // the pprof's period; NULL for none
} ddog_prof_DatadogOptions;

// One OTLP profile to write: the profile's sample type `source`, under the
// sanctioned `sample_type`, with an optional period (NULL for none).
typedef struct ddog_prof_OtlpProfile
{
    ddog_prof_SampleTypeId source;
    ddog_prof_OtlpSampleType sample_type;
    const ddog_prof_Period* period;
} ddog_prof_OtlpProfile;

// How a request is encoded; shared by every request builder. A NULL pointer
// means the defaults: max_bytes 0 (20 MiB) and AUTO compression (zstd for
// pprof when built in, none for OTLP).
typedef struct ddog_prof_EncodeOptions
{
    size_t max_bytes;
    ddog_prof_Compression compression;
} ddog_prof_EncodeOptions;

typedef struct ddog_prof_Scope
{
    ddog_prof_Str name;
    ddog_prof_Str version;
    const ddog_prof_Tag* attributes;
    size_t attributes_len;
} ddog_prof_Scope;

// A static description of status.
const char* ddog_prof_Status_message(ddog_prof_Status status);

// Tables. lock_dir is required on Unix (the caller chooses, e.g. the temp
// directory) and must be NULL on Windows.
ddog_prof_Status ddog_prof_StringTable_new(uint8_t regions,
                                           size_t arena_bytes,
                                           const char* lock_dir,
                                           ddog_prof_StringTable** out);
// Like ddog_prof_StringTable_new, and also interns reserved, writing
// reserved[i]'s id to ids[i]: placeholders a runtime falls back to when
// interning is busy later. INVALID_ARGUMENT unless reserved_len == ids_len.
// On error no table is made and ids is all zeros.
ddog_prof_Status ddog_prof_StringTable_new_with_reserved(uint8_t regions,
                                                        size_t arena_bytes,
                                                        const char* lock_dir,
                                                        const ddog_prof_Str* reserved,
                                                        size_t reserved_len,
                                                        uint32_t* ids,
                                                        size_t ids_len,
                                                        ddog_prof_StringTable** out);
ddog_prof_Status ddog_prof_StringTable_clone(ddog_prof_StringTable* table, ddog_prof_StringTable** out);
void ddog_prof_StringTable_drop(ddog_prof_StringTable* table);
ddog_prof_Status ddog_prof_StringTable_intern(ddog_prof_StringTable* table, ddog_prof_Str s, uint32_t* out);

// Fork support: the tables are shared with children (Unix), so a child keeps
// using them. prepare_fork takes a table's thread lock (waiting for it), so a
// child cannot inherit it held by another thread; call finish_fork once in the
// parent and once in the child afterwards. Prepare tables in a fixed order.
ddog_prof_Status ddog_prof_StringTable_prepare_fork(ddog_prof_StringTable* table);
ddog_prof_Status ddog_prof_StringTable_finish_fork(ddog_prof_StringTable* table);
ddog_prof_Status ddog_prof_FunctionTable_prepare_fork(ddog_prof_FunctionTable* table);
ddog_prof_Status ddog_prof_FunctionTable_finish_fork(ddog_prof_FunctionTable* table);

ddog_prof_Status ddog_prof_FunctionTable_new(uint8_t regions, const char* lock_dir, ddog_prof_FunctionTable** out);
// Like ddog_prof_FunctionTable_new, and also interns reserved (functions
// whose fields are string ids), writing reserved[i]'s id to ids[i]. See
// ddog_prof_StringTable_new_with_reserved.
ddog_prof_Status ddog_prof_FunctionTable_new_with_reserved(uint8_t regions,
                                                          const char* lock_dir,
                                                          const ddog_prof_Function* reserved,
                                                          size_t reserved_len,
                                                          uint32_t* ids,
                                                          size_t ids_len,
                                                          ddog_prof_FunctionTable** out);
ddog_prof_Status ddog_prof_FunctionTable_clone(ddog_prof_FunctionTable* table, ddog_prof_FunctionTable** out);
void ddog_prof_FunctionTable_drop(ddog_prof_FunctionTable* table);
ddog_prof_Status ddog_prof_FunctionTable_intern(ddog_prof_FunctionTable* table,
                                                const ddog_prof_Function* function,
                                                uint32_t* out);

// Profiles. The profile takes its own handles to the tables.
ddog_prof_Status ddog_prof_Profile_new(ddog_prof_StringTable* strings,
                                       ddog_prof_FunctionTable* functions,
                                       const ddog_prof_SampleType* types,
                                       size_t types_len,
                                       int64_t start_ns,
                                       ddog_prof_Profile** out);
void ddog_prof_Profile_drop(ddog_prof_Profile* profile);
// INVALID_ARGUMENT if a value's sample type is out of range; the profile is
// then unchanged. Each sample type may appear at most once: debug builds
// reject a repeated type the same way, release builds do not check.
ddog_prof_Status ddog_prof_Profile_add(ddog_prof_Profile* profile, const ddog_prof_Sample* sample);
// Endpoint names are kept by the profile until its next reset, never in the
// shared string table.
ddog_prof_Status ddog_prof_Profile_set_endpoint(ddog_prof_Profile* profile,
                                                uint64_t local_root_span_id,
                                                ddog_prof_Str endpoint);
ddog_prof_Status ddog_prof_Profile_add_endpoint_count(ddog_prof_Profile* profile,
                                                      ddog_prof_Str endpoint,
                                                      int64_t count);
ddog_prof_Status ddog_prof_Profile_reset(ddog_prof_Profile* profile, int64_t start_ns);

// Requests. See ddog_prof_EncodeOptions for encode. The profile is unchanged, so both
// may be built from the same profile.
//
// Each format is a cargo feature of libdd-profiling-ffi (both on by
// default, with `zstd`): ddog_prof_Profile_datadog_request and
// ddog_prof_Profile_encode_pprof exist only with `pprof`,
// ddog_prof_Profile_otlp_request only with `otel`. Rust dependents learn which
// were built in from DEP_DATADOG_PROFILING_FFI_PPROF, _OTEL and _ZSTD ("1"/"0").
ddog_prof_Status ddog_prof_Profile_datadog_request(const ddog_prof_Profile* profile,
                                                   int64_t end_ns,
                                                   const ddog_prof_Resource* resource,
                                                   const ddog_prof_DatadogOptions* options,
                                                   const ddog_prof_EncodeOptions* encode, // nullable
                                                   ddog_prof_Request** out);
// One OTLP profile per entry of profiles, and nothing else: NOTHING_TO_ENCODE
// for none, INVALID_ARGUMENT for a source out of range or with the wrong
// unit, or a sanctioned type requested twice.
ddog_prof_Status ddog_prof_Profile_otlp_request(const ddog_prof_Profile* profile,
                                                int64_t end_ns,
                                                const ddog_prof_Resource* resource,
                                                const ddog_prof_Scope* scope,
                                                const ddog_prof_OtlpProfile* profiles,
                                                size_t profiles_len,
                                                const ddog_prof_EncodeOptions* encode, // nullable
                                                ddog_prof_Request** out);

// pprof without the intake envelope, for writing to a file, compressed as
// compression says (AUTO as for the Datadog request): read it with
// ddog_prof_Request_body.
ddog_prof_Status ddog_prof_Profile_encode_pprof(const ddog_prof_Profile* profile,
                                                int64_t end_ns,
                                                const ddog_prof_Period* period, // nullable
                                                const ddog_prof_EncodeOptions* encode, // nullable
                                                ddog_prof_Request** out);

ddog_prof_Str ddog_prof_Request_method(const ddog_prof_Request* request);
// Relative to the destination's base URL: the agent or intake for Datadog,
// the collector's OTLP/HTTP endpoint for OTLP.
ddog_prof_Str ddog_prof_Request_path(const ddog_prof_Request* request);
size_t ddog_prof_Request_header_count(const ddog_prof_Request* request);
ddog_prof_Status ddog_prof_Request_header(const ddog_prof_Request* request,
                                          size_t index,
                                          ddog_prof_Str* name,
                                          ddog_prof_Str* value);
ddog_prof_Bytes ddog_prof_Request_body(const ddog_prof_Request* request);
void ddog_prof_Request_drop(ddog_prof_Request* request);

#ifdef __cplusplus
}
#endif

#endif // DDOG_PROFILING_V2_H
