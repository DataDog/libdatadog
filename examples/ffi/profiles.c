// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/common.h>
#include <datadog/profiling.h>
#include <stdio.h>
#include <stdlib.h>

int main(void) {
  const ddog_prof_ValueType wall_time = {
      .type_ = DDOG_CHARSLICE_C("wall-time"),
      .unit = DDOG_CHARSLICE_C("nanoseconds"),
  };
  const ddog_prof_Slice_ValueType sample_types = {&wall_time, 1};
  const ddog_prof_Period period = {wall_time, 60};

  ddog_prof_Profile_NewResult new_result = ddog_prof_Profile_new(sample_types, &period);
  if (new_result.tag != DDOG_PROF_PROFILE_NEW_RESULT_OK) {
    ddog_CharSlice message = ddog_Error_message(&new_result.err);
    fprintf(stderr, "%.*s", (int)message.len, message.ptr);
    ddog_Error_drop(&new_result.err);
    exit(EXIT_FAILURE);
  }

  ddog_prof_Profile *profile = &new_result.ok;

  ddog_prof_Location root_location = {
      // yes, a zero-initialized mapping is valid
      .mapping = (ddog_prof_Mapping){0},
      .function =
          (struct ddog_prof_Function){
              .name = DDOG_CHARSLICE_C("{main}"),
              .filename = DDOG_CHARSLICE_C("/srv/example/index.php"),
          },
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

    ddog_prof_Profile_Result add_result = ddog_prof_Profile_add(profile, sample, 0);
    if (add_result.tag != DDOG_PROF_PROFILE_RESULT_OK) {
      ddog_CharSlice message = ddog_Error_message(&add_result.err);
      fprintf(stderr, "%.*s", (int)message.len, message.ptr);
      ddog_Error_drop(&add_result.err);
    }
  }

  //   printf("Press any key to reset and drop...");
  //   getchar();

  ddog_prof_Profile_Result reset_result = ddog_prof_Profile_reset(profile);
  if (reset_result.tag != DDOG_PROF_PROFILE_RESULT_OK) {
    ddog_CharSlice message = ddog_Error_message(&reset_result.err);
    fprintf(stderr, "%.*s", (int)message.len, message.ptr);
    ddog_Error_drop(&reset_result.err);
  }
  ddog_prof_Profile_drop(profile);


  return 0;
}