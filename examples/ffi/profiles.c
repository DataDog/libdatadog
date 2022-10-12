// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present
// Datadog, Inc.

#include <assert.h>
#include <datadog/common.h>
#include <datadog/profiling.h>
#include <stdint.h>
#include <time.h>

/* Creates a profile with one sample type "wall-time" with period of
 * "wall-time" with unit 60 "nanoseconds". Adds one sample with a string label
 * "language".
 */
int main(void) {
  const struct ddog_ValueType wall_time = {
      .type_ = DDOG_CHARSLICE_C("wall-time"),
      .unit = DDOG_CHARSLICE_C("nanoseconds"),
  };
  const struct ddog_Slice_value_type sample_types = {&wall_time, 1};
  const struct ddog_Period period = {wall_time, 60};

  ddog_Profile *profile = ddog_Profile_new(sample_types, &period, NULL);

  struct ddog_Line root_line = {
      .function =
          (struct ddog_Function){
              .name = DDOG_CHARSLICE_C("{main}"),
              .filename = DDOG_CHARSLICE_C("/srv/example/index.php"),
          },
      .line = 0,
  };

  struct ddog_Location root_location = {
      // yes, a zero-initialized mapping is valid
      .mapping = (struct ddog_Mapping){0},
      .lines = (struct ddog_Slice_line){&root_line, 1},
  };
  int64_t value = 10;
  const struct ddog_Label label = {
      .key = DDOG_CHARSLICE_C("language"),
      .str = DDOG_CHARSLICE_C("php"),
  };

  struct timespec now;
  int result = timespec_get(&now, TIME_UTC);
  assert(result == TIME_UTC);

  int64_t tick =
      ((int64_t)now.tv_sec) * INT64_C(1000000000) + (int64_t)now.tv_nsec;

  struct ddog_Sample sample = {
      .locations = {&root_location, 1},
      .values = {&value, 1},
      .labels = {&label, 1},
      .tick = tick,
  };
  ddog_Profile_add(profile, sample);
  ddog_Profile_free(profile);
  return 0;
}
