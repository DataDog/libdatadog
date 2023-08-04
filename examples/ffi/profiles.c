// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present
// Datadog, Inc.

#include <datadog/common.h>
#include <datadog/profiling.h>
#include <stdint.h>
#include <stdio.h>

int main(void) {
  const ddog_prof_ValueType wall_time = {
      .type_ = DDOG_CHARSLICE_C("wall-time"),
      .unit = DDOG_CHARSLICE_C("nanoseconds"),
  };
  const ddog_prof_Slice_ValueType sample_types = {&wall_time, 1};
  const ddog_prof_Period period = {wall_time, 60};

  ddog_prof_Profile *profile = ddog_prof_Profile_new(sample_types, &period, NULL);

  ddog_prof_Line root_line = {
      .function =
          (struct ddog_prof_Function) {
              .name = DDOG_CHARSLICE_C("{main}"),
              .filename = DDOG_CHARSLICE_C("/srv/example/index.php"),
          },
      .line = 0,
  };

  ddog_prof_Location root_location = {
      // yes, a zero-initialized mapping is valid
      .mapping = (ddog_prof_Mapping) {0},
      .lines = (ddog_prof_Slice_Line) {&root_line, 1},
  };
  int64_t value = 10;
  ddog_prof_Label label = {
      .key = DDOG_CHARSLICE_C("unique_counter"),
      .num = 0,
  };
  const ddog_prof_Sample sample = {
      .locations = {&root_location, 1},
      .values = {&value, 1},
      .labels = {&label, 1},
  };

  for (int i = 0; i < 10000000; i++) {
      label.num = i;

      ddog_prof_Profile_AddResult add_result = ddog_prof_Profile_add(profile, sample);
      if (add_result.tag != DDOG_PROF_PROFILE_ADD_RESULT_OK) {
        ddog_CharSlice message = ddog_Error_message(&add_result.err);
        fprintf(stderr, "%*s", (int)message.len, message.ptr);
        ddog_Error_drop(&add_result.err);
      }
 }

//   printf("Press any key to reset and drop...");
//   getchar();

//   ddog_prof_Profile_reset(profile, NULL);
//   ddog_prof_Profile_drop(profile);

  printf("Press any key to exit...");
  getchar();

  return 0;
}