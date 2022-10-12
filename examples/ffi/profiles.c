// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present
// Datadog, Inc.

#include <assert.h>
#include <datadog/common.h>
#include <datadog/profiling.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <time.h>

/* The profile built doesn't match the same format as the PHP profiler, but
 * it is similar and should make sense.
 */
int main(void) {
  const struct ddog_ValueType sample_types_data[] = {
      {
          .type_ = DDOG_CHARSLICE_C("wall-time"),
          .unit = DDOG_CHARSLICE_C("nanoseconds"),
      },
      {
          .type_ = DDOG_CHARSLICE_C("cpu-time"),
          .unit = DDOG_CHARSLICE_C("nanoseconds"),
      },
  };

  const struct ddog_Slice_value_type sample_types = {sample_types_data, 2};
  const struct ddog_Period period = {sample_types_data[0], INT64_C(60000000000)};

  ddog_Profile *profile = ddog_Profile_new(sample_types, &period, NULL);

  struct ddog_Line lines[] = {
      {
          .function =
              (struct ddog_Function){
                  .name = DDOG_CHARSLICE_C("sleep"),
              },
          .line = 0,
      },
      {
          .function =
              (struct ddog_Function){
                  .name = DDOG_CHARSLICE_C("<?php"),
                  .filename = DDOG_CHARSLICE_C("/srv/example.org/index.php"),
              },
          .line = 3,
      },
  };

  struct ddog_Location locations[] = {
      {
          .mapping = (struct ddog_Mapping){.filename = DDOG_CHARSLICE_C("[ext/standard]")},
          .lines = (struct ddog_Slice_line){&lines[0], 1},
      },
      {
          // yes, a zero-initialized mapping is valid
          .mapping = (struct ddog_Mapping){0},
          .lines = (struct ddog_Slice_line){&lines[1], 1},
      },
  };
  int64_t values[] = {10000, 73};
  const struct ddog_Label label = {
      .key = DDOG_CHARSLICE_C("process_id"),
      .str = DDOG_CHARSLICE_C("12345"),
  };

  struct timespec now;
  int timespec_get_result = timespec_get(&now, TIME_UTC);
  assert(timespec_get_result == TIME_UTC);

  int64_t tick = ((int64_t)now.tv_sec) * INT64_C(1000000000) + (int64_t)now.tv_nsec;

  struct ddog_Sample sample = {
      .locations = {locations, 2},
      .values = {values, 2},
      .labels = {&label, 1},
      .tick = tick,
  };
  uint64_t sample_id1 = ddog_Profile_add(profile, sample);
  uint64_t sample_id2 = ddog_Profile_add(profile, sample);
  if (sample_id1 != sample_id2) {
    fprintf(stderr, "Sample ids did not match: %" PRIu64 " != %" PRIu64, sample_id1, sample_id2);
  }

  int exit_code = 0;
  struct ddog_SerializeResult result = ddog_Profile_serialize(profile, NULL, NULL);
  if (result.tag == DDOG_SERIALIZE_RESULT_OK) {
    struct ddog_EncodedProfile encoded_profile = result.ok;
    struct ddog_Vec_u8 buffer = encoded_profile.buffer;
    size_t bytes_written = fwrite(buffer.ptr, 1, buffer.len, stdout);
    if (bytes_written != buffer.len) {
      fprintf(stderr, "Only wrote %zu of %zu bytes.\n", bytes_written, buffer.len);
      exit_code = 1;
    }
  } else {
    struct ddog_Vec_u8 err = result.err;
    fprintf(stderr, "Failed to serialize profile: %*s.\n", (int)err.len, (const char *)err.ptr);
    exit_code = 1;
  }
  ddog_SerializeResult_drop(result);
  ddog_Profile_free(profile);
  return exit_code;
}
