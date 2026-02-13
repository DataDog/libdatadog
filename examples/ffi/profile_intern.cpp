// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

extern "C" {
#include <datadog/profiling.h>
}

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <iostream>

static ddog_CharSlice to_slice(const char *s) { return {.ptr = s, .len = strlen(s)}; }

static void print_error(const ddog_Error &err) {
  auto charslice = ddog_Error_message(&err);
  fprintf(stderr, "%.*s\n", static_cast<int>(charslice.len), charslice.ptr);
}

static void check_status(ddog_prof_Status status, const char *context) {
  if (status != DDOG_PROF_STATUS_OK) {
    fprintf(stderr, "%s failed with status=%d\n", context, static_cast<int>(status));
    exit(EXIT_FAILURE);
  }
}

static ddog_prof_StringId2 insert_string(ddog_prof_ProfilesDictionary *dict, const char *s) {
  ddog_prof_StringId2 out = DDOG_PROF_STRINGID2_EMPTY;
  check_status(ddog_prof_ProfilesDictionary_insert_str(
                   &out, dict, to_slice(s), DDOG_PROF_UTF8OPTION_ASSUME),
               "ddog_prof_ProfilesDictionary_insert_str");
  return out;
}

int main(void) {
  ddog_prof_ProfilesDictionaryHandle dict_handle = {};
  check_status(ddog_prof_ProfilesDictionary_new(&dict_handle), "ddog_prof_ProfilesDictionary_new");
  auto *dict = ddog_prof_ProfilesDictionaryHandle_as_ref(&dict_handle);

  const ddog_prof_SampleType wall_time = DDOG_PROF_SAMPLE_TYPE_WALL_TIME;
  const ddog_prof_Slice_SampleType sample_types = {&wall_time, 1};
  const ddog_prof_Period period = {.sample_type = wall_time, .value = 60};

  ddog_prof_Profile profile = {};
  check_status(ddog_prof_Profile_with_dictionary(&profile, &dict_handle, sample_types, &period),
               "ddog_prof_Profile_with_dictionary");

  ddog_prof_StringId2 fn_name = insert_string(dict, "{main}");
  ddog_prof_StringId2 file_name = insert_string(dict, "/srv/example/index.php");
  ddog_prof_StringId2 magic_key = insert_string(dict, "magic_word");
  ddog_prof_StringId2 unique_counter = insert_string(dict, "unique_counter");

  ddog_prof_Mapping2 mapping = {
      .memory_start = 0, .memory_limit = 0, .file_offset = 0, .filename = file_name, .build_id = DDOG_PROF_STRINGID2_EMPTY};
  ddog_prof_MappingId2 mapping_id = {};
  check_status(
      ddog_prof_ProfilesDictionary_insert_mapping(&mapping_id, dict, &mapping),
      "ddog_prof_ProfilesDictionary_insert_mapping");

  ddog_prof_Function2 function = {
      .name = fn_name, .system_name = DDOG_PROF_STRINGID2_EMPTY, .file_name = file_name};
  ddog_prof_FunctionId2 function_id = {};
  check_status(
      ddog_prof_ProfilesDictionary_insert_function(&function_id, dict, &function),
      "ddog_prof_ProfilesDictionary_insert_function");

  ddog_prof_Location2 location = {.mapping = mapping_id, .function = function_id, .address = 0, .line = 0};
  ddog_Slice_Location2 locations = {.ptr = &location, .len = 1};

  auto start = std::chrono::system_clock::now();
  for (int64_t i = 1; i <= 100000; i++) {
    ddog_prof_Label2 labels[2] = {
        {.key = magic_key, .str = to_slice("abracadabra"), .num = 0, .num_unit = to_slice("")},
        {.key = unique_counter, .str = to_slice(""), .num = i, .num_unit = to_slice("")},
    };
    ddog_Slice_I64 values = {.ptr = &i, .len = 1};
    ddog_prof_Sample2 sample = {
        .locations = locations, .values = values, .labels = {.ptr = labels, .len = 2}};
    check_status(ddog_prof_Profile_add2(&profile, sample, i),
                 "ddog_prof_Profile_add2");
  }
  auto end = std::chrono::system_clock::now();
  std::chrono::duration<double> elapsed_seconds = end - start;
  std::cout << "elapsed time: " << elapsed_seconds.count() << "s" << std::endl;

  ddog_prof_Profile_drop(&profile);
  ddog_prof_ProfilesDictionary_drop(&dict_handle);
  return EXIT_SUCCESS;
}
