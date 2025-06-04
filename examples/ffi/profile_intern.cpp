// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

extern "C" {
#include <datadog/common.h>
#include <datadog/crashtracker.h>
#include <datadog/profiling.h>
}
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <memory>
#include <optional>
#include <string>
#include <thread>
#include <vector>

static ddog_CharSlice to_slice_c_char(const char *s) { return {.ptr = s, .len = strlen(s)}; }
static ddog_CharSlice to_slice_c_char(const char *s, std::size_t size) {
  return {.ptr = s, .len = size};
}
static ddog_CharSlice to_slice_string(std::string const &s) {
  return {.ptr = s.data(), .len = s.length()};
}

static std::string to_string(ddog_CharSlice s) { return std::string(s.ptr, s.len); }

void print_error(const ddog_Error &err) {
  auto charslice = ddog_Error_message(&err);
  printf("%.*s\n", static_cast<int>(charslice.len), charslice.ptr);
}

#define CHECK_RESULT(typ, ok_tag)                                                                  \
  void check_result(typ result) {                                                                  \
    if (result.tag != ok_tag) {                                                                    \
      print_error(result.err);                                                                     \
      ddog_Error_drop(&result.err);                                                                \
      exit(EXIT_FAILURE);                                                                          \
    }                                                                                              \
  }

CHECK_RESULT(ddog_VoidResult, DDOG_VOID_RESULT_OK)

#define EXTRACT_RESULT(typ, uppercase)                                                             \
  ddog_prof_##typ##Id extract_result(ddog_prof_##typ##Id_Result result) {                          \
    if (result.tag != DDOG_PROF_##uppercase##_ID_RESULT_OK_GENERATIONAL_ID_##uppercase##_ID) {     \
      print_error(result.err);                                                                     \
      ddog_Error_drop(&result.err);                                                                \
      exit(EXIT_FAILURE);                                                                          \
    } else {                                                                                       \
      return result.ok;                                                                            \
    }                                                                                              \
  }

EXTRACT_RESULT(Function, FUNCTION)
EXTRACT_RESULT(Label, LABEL)
EXTRACT_RESULT(LabelSet, LABEL_SET)
EXTRACT_RESULT(Location, LOCATION)
EXTRACT_RESULT(Mapping, MAPPING)
EXTRACT_RESULT(StackTrace, STACK_TRACE)
EXTRACT_RESULT(String, STRING)

void wait_for_user(std::string s) {
  std::cout << s << std::endl;
  getchar();
}

int main(void) {
  const ddog_prof_ValueType wall_time = {
      .type_ = to_slice_c_char("wall-time"),
      .unit = to_slice_c_char("nanoseconds"),
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
  auto root_function_name =
      extract_result(ddog_prof_Profile_intern_string(profile, to_slice_c_char("{main}")));
  auto root_file_name = extract_result(
      ddog_prof_Profile_intern_string(profile, to_slice_c_char("/srv/example/index.php")));
  auto root_mapping = extract_result(
      ddog_prof_Profile_intern_mapping(profile, 0, 0, 0, root_file_name, ddog_INTERNED_EMPTY_STRING));
  auto root_function = extract_result(ddog_prof_Profile_intern_function(
      profile, root_function_name, ddog_INTERNED_EMPTY_STRING, root_file_name));
  auto root_location = extract_result(ddog_prof_Profile_intern_location_with_mapping_id(
      profile, root_mapping, root_function, 0, 0));
  ddog_prof_Slice_LocationId locations = {.ptr = &root_location, .len = 1};
  auto stacktrace = extract_result(ddog_prof_Profile_intern_stacktrace(profile, locations));

  auto magic_label_key =
      extract_result(ddog_prof_Profile_intern_string(profile, to_slice_c_char("magic_word")));
  auto magic_label_val =
      extract_result(ddog_prof_Profile_intern_string(profile, to_slice_c_char("abracadabra")));
  auto magic_label =
      extract_result(ddog_prof_Profile_intern_label_str(profile, magic_label_key, magic_label_val));

  // Keep this id around, no need to reintern the same string over and over again.
  auto counter_id =
      extract_result(ddog_prof_Profile_intern_string(profile, to_slice_c_char("unique_counter")));

  // wait_for_user("Press any key to start adding values ...");

  std::chrono::time_point<std::chrono::system_clock> start = std::chrono::system_clock::now();
  for (auto i = 0; i < 10000000; i++) {
    auto counter_label = extract_result(ddog_prof_Profile_intern_label_num(profile, counter_id, i));
    ddog_prof_LabelId label_array[2] = {magic_label, counter_label};
    ddog_prof_Slice_LabelId label_slice = {.ptr = label_array, .len = 2};
    auto labels = extract_result(ddog_prof_Profile_intern_labelset(profile, label_slice));

    int64_t value = i * 10;
    ddog_Slice_I64 values = {.ptr = &value, .len = 1};
    int64_t timestamp = 3 + 800 * i;
    check_result(ddog_prof_Profile_intern_sample(profile, stacktrace, values, labels, timestamp));
  }
  std::chrono::time_point<std::chrono::system_clock> end = std::chrono::system_clock::now();
  std::chrono::duration<double> elapsed_seconds = end - start;
  std::cout << "elapsed time: " << elapsed_seconds.count() << "s" << std::endl;

  // wait_for_user("Press any key to reset and drop...");

  ddog_prof_Profile_Result reset_result = ddog_prof_Profile_reset(profile);
  if (reset_result.tag != DDOG_PROF_PROFILE_RESULT_OK) {
    ddog_CharSlice message = ddog_Error_message(&reset_result.err);
    fprintf(stderr, "%.*s", (int)message.len, message.ptr);
    ddog_Error_drop(&reset_result.err);
  }
  ddog_prof_Profile_drop(profile);

  // wait_for_user("Press any key to exit...");

  return EXIT_SUCCESS;
}
