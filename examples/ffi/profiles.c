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

  // Create a ProfilesDictionary for the new API
  ddog_prof_ProfilesDictionaryHandle dict = {0};
  ddog_prof_Status dict_status = ddog_prof_ProfilesDictionary_new(&dict);
  if (dict_status.flags != 0) {
    fprintf(stderr, "Failed to create dictionary: %s\n", dict_status.err);
    ddog_prof_Status_drop(&dict_status);
    exit(EXIT_FAILURE);
  }

  // Create profile using the dictionary
  ddog_prof_Profile profile = {0};
  ddog_prof_Status profile_status =
      ddog_prof_Profile_with_dictionary(&profile, &dict, sample_types, &period);
  if (profile_status.flags != 0) {
    fprintf(stderr, "Failed to create profile: %s\n", profile_status.err);
    ddog_prof_Status_drop(&profile_status);
    ddog_prof_ProfilesDictionary_drop(&dict);
    exit(EXIT_FAILURE);
  }

  // Original API sample
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

    ddog_prof_Profile_Result add_result = ddog_prof_Profile_add(&profile, sample, 0);
    if (add_result.tag != DDOG_PROF_PROFILE_RESULT_OK) {
      ddog_CharSlice message = ddog_Error_message(&add_result.err);
      fprintf(stderr, "%.*s", (int)message.len, message.ptr);
      ddog_Error_drop(&add_result.err);
    }
  }

  // New API sample using the dictionary
  // Insert strings into the dictionary
  ddog_prof_StringId2 function_name_id, filename_id, label_key_id;

  dict_status = ddog_prof_ProfilesDictionary_insert_str(
      &function_name_id, dict, DDOG_CHARSLICE_C("{main}"), DDOG_PROF_UTF8_OPTION_ASSUME);
  if (dict_status.flags != 0) {
    fprintf(stderr, "Failed to insert function name: %s\n", dict_status.err);
    ddog_prof_Status_drop(&dict_status);
    goto cleanup;
  }

  dict_status = ddog_prof_ProfilesDictionary_insert_str(&filename_id, dict,
                                                        DDOG_CHARSLICE_C("/srv/example/index.php"),
                                                        DDOG_PROF_UTF8_OPTION_ASSUME);
  if (dict_status.flags != 0) {
    fprintf(stderr, "Failed to insert filename: %s\n", dict_status.err);
    ddog_prof_Status_drop(&dict_status);
    goto cleanup;
  }

  dict_status = ddog_prof_ProfilesDictionary_insert_str(
      &label_key_id, dict, DDOG_CHARSLICE_C("unique_counter"), DDOG_PROF_UTF8_OPTION_ASSUME);
  if (dict_status.flags != 0) {
    fprintf(stderr, "Failed to insert label key: %s\n", dict_status.err);
    ddog_prof_Status_drop(&dict_status);
    goto cleanup;
  }

  // Create a function using the dictionary IDs
  ddog_prof_FunctionId2 function_id;
  ddog_prof_Function2 function2 = {
      .name = function_name_id,
      .system_name = DDOG_PROF_STRINGID2_EMPTY,
      .file_name = filename_id,
  };

  dict_status = ddog_prof_ProfilesDictionary_insert_function(&function_id, dict, &function2);
  if (dict_status.flags != 0) {
    fprintf(stderr, "Failed to insert function: %s\n", dict_status.err);
    ddog_prof_Status_drop(&dict_status);
    goto cleanup;
  }

  // Create a location using the dictionary IDs
  ddog_prof_Location2 location2 = {
      .mapping = (ddog_prof_MappingId2){0}, // null mapping is valid
      .function = function_id,
      .address = 0,
      .line = 0,
  };

  // Create a label using dictionary IDs
  ddog_prof_Label2 label2 = {
      .key = label_key_id,
      .str = DDOG_CHARSLICE_C(""),
      .num = 0,
      .num_unit = DDOG_CHARSLICE_C(""),
  };

  // Add samples using the new API
  for (int i = 0; i < 10000000; i++) {
    label2.num = i;

    ddog_prof_Sample2 sample2 = {
        .locations = {&location2, 1},
        .values = {&value, 1},
        .labels = {&label2, 1},
    };

    ddog_prof_Status add2_status = ddog_prof_Profile_add2(&profile, sample2, 0);
    if (add2_status.flags != 0) {
      fprintf(stderr, "add2 error: %s\n", add2_status.err);
      ddog_prof_Status_drop(&add2_status);
    }
  }

  //   printf("Press any key to reset and drop...");
  //   getchar();

cleanup:
  ; // Can't have a declaration after a label pre-C23, so use an empty statement.
  ddog_prof_Profile_Result reset_result = ddog_prof_Profile_reset(&profile);
  if (reset_result.tag != DDOG_PROF_PROFILE_RESULT_OK) {
    ddog_CharSlice message = ddog_Error_message(&reset_result.err);
    fprintf(stderr, "%.*s", (int)message.len, message.ptr);
    ddog_Error_drop(&reset_result.err);
  }
  ddog_prof_Profile_drop(&profile);

  // Drop the dictionary
  ddog_prof_ProfilesDictionary_drop(&dict);

  return 0;
}
