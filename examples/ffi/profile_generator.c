// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/common.h>
#include <datadog/profiling.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

const int max_depth = 300;
const int min_depth = 5;

const int max_samples = 180000;
const int min_samples = 5000;

const int max_unique_locations = 5000;
const int min_unique_locations = 500;

int random_between(unsigned int min, unsigned int max) {
  return (rand() % (max - min)) + min;
}

void generate_profile(uint64_t iteration_id, ddog_prof_Profile *profile, ddog_prof_Location *locations);

int main(void) {
  const ddog_prof_ValueType wall_time = {
      .type_ = DDOG_CHARSLICE_C("cpu-time"),
      .unit = DDOG_CHARSLICE_C("nanoseconds"),
  };
  const ddog_prof_ValueType cpu_time = {
      .type_ = DDOG_CHARSLICE_C("wall-time"),
      .unit = DDOG_CHARSLICE_C("nanoseconds"),
  };
  ddog_prof_ValueType types[2] = {cpu_time, wall_time};
  const ddog_prof_Slice_ValueType sample_types = {types, 2};

  ddog_prof_Profile_NewResult new_result = ddog_prof_Profile_new(sample_types, NULL, NULL);

  if (new_result.tag != DDOG_PROF_PROFILE_NEW_RESULT_OK) {
    ddog_CharSlice message = ddog_Error_message(&new_result.err);
    fprintf(stderr, "%.*s\n", (int)message.len, message.ptr);
    abort();
  }

  ddog_prof_Profile *profile = &new_result.ok;
  ddog_prof_Location locations[max_depth];

  for (uint64_t iteration_id = 0; iteration_id < UINT64_MAX; iteration_id++) {
    generate_profile(iteration_id, profile, locations);
  }

  printf("Press any key to exit...");
  getchar();

  return 0;
}

void generate_profile(uint64_t iteration_id, ddog_prof_Profile *profile, ddog_prof_Location *locations) {
  srand(iteration_id);

  int samples = random_between(min_samples, max_samples);
  int unique_locations = random_between(min_unique_locations, max_unique_locations);

  printf("Iteration %lu => samples %d, unique_locations %d\n", iteration_id, samples, unique_locations);

  int current_location = 0;

  char all_names[unique_locations][100];
  char all_filenames[unique_locations][100];

  for (int i = 0; i < unique_locations; i++) {
    snprintf(all_names[i], 100, "name_%d", i);
    snprintf(all_filenames[i], 100, "filename_%d", i);
  }

  for (int sample = 0; sample < samples; sample++) {
    int depth = random_between(min_depth, max_depth);

    for (int current_depth = 0; current_depth < depth; current_depth++) {
      locations[current_depth] = (ddog_prof_Location) {
        .mapping = {.filename = DDOG_CHARSLICE_C(""), .build_id = DDOG_CHARSLICE_C("")},
        .function = (ddog_prof_Function) {.name = {.ptr = all_names[current_location], .len = strlen(all_names[current_location])}, .filename = {.ptr = all_filenames[current_location], .len = strlen(all_filenames[current_location])}},
        .line = current_location,
      };

      current_location++;
      if (current_location >= unique_locations) {
        current_location = 0;
      }
    }

    int another_value = random_between(0, 1000);
    int64_t values[2] = {another_value, 2};

    ddog_prof_Label label = {
      .key = DDOG_CHARSLICE_C("unique_counter"),
      .num = another_value,
    };

    const ddog_prof_Sample this_sample = {
        .locations = {locations, depth},
        .values = {values, 2},
        .labels = {&label, 1},
    };

    ddog_prof_Profile_Result add_result = ddog_prof_Profile_add(profile, this_sample, another_value);
    if (add_result.tag != DDOG_PROF_PROFILE_RESULT_OK) {
      ddog_CharSlice message = ddog_Error_message(&add_result.err);
      fprintf(stderr, "%.*s\n", (int)message.len, message.ptr);
      abort();
    }

    if (sample % 40 == 0) current_location = 0;
  }

  ddog_prof_Profile_SerializeResult result = ddog_prof_Profile_serialize(profile, NULL, NULL, NULL);
  if (result.tag != DDOG_PROF_PROFILE_SERIALIZE_RESULT_OK) {
    ddog_CharSlice message = ddog_Error_message(&result.err);
    fprintf(stderr, "%.*s\n", (int)message.len, message.ptr);
    abort();
  }

  // Write encodedprofile buffer to file
  char pathname[100];
  snprintf(pathname, 100, "profile_%lu.pprof", iteration_id);
  FILE *file = fopen(pathname, "wb");
  fwrite(result.ok.buffer.ptr, 1, result.ok.buffer.len, file);
  fclose(file);

  ddog_prof_EncodedProfile_drop(&result.ok);
}
