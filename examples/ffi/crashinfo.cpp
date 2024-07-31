// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

extern "C" {
#include <datadog/common.h>
#include <datadog/crashtracker.h>
}
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <string>
#include <thread>
#include <vector>

static ddog_CharSlice to_slice_c_char(const char *s) { return {.ptr = s, .len = strlen(s)}; }
static ddog_CharSlice to_slice_string(std::string &s) {
  return {.ptr = s.data(), .len = s.length()};
}

// TODO: Testing on my mac, the tags appear to have the opposite meaning you'd
// expect
static ddog_crasht_Option_U32 some_u32(uint32_t i) {
  ddog_crasht_Option_U32 rval = {.tag = DDOG_CRASHT_OPTION_U32_SOME_U32};
  rval.some = i;
  return rval;
}
static ddog_crasht_Option_U32 none_u32() {
  return {.tag = DDOG_CRASHT_OPTION_U32_NONE_U32};
}

struct Deleter {
  void operator()(ddog_crasht_CrashInfo *object) { ddog_crasht_CrashInfo_drop(object); }
};

void print_error(const char *s, const ddog_Error &err) {
  auto charslice = ddog_Error_message(&err);
  printf("%s (%.*s)\n", s, static_cast<int>(charslice.len), charslice.ptr);
}

void check_result(ddog_crasht_Result result, const char *msg) {
  if (result.tag != DDOG_CRASHT_RESULT_OK) {
    print_error(msg, result.err);
    ddog_Error_drop(&result.err);
    exit(EXIT_FAILURE);
  }
}

void add_stacktrace(std::unique_ptr<ddog_crasht_CrashInfo, Deleter> &crashinfo) {

  // Collect things into vectors so they stay alive till the function exits
  std::vector<std::string> filenames;
  std::vector<std::string> function_names;
  for (uintptr_t i = 0; i < 20; ++i) {
    filenames.push_back("/path/to/code/file_" + std::to_string(i));
    function_names.push_back("func_" + std::to_string(i));
  }

  std::vector<ddog_crasht_StackFrameNames> names;
  for (uintptr_t i = 0; i < 20; ++i) {
    names.push_back({.colno = some_u32(i),
                     .filename = to_slice_string(filenames[i]),
                     .lineno = some_u32(2 * i + 3),
                     .name = to_slice_string(function_names[i])});
  }

  std::vector<ddog_crasht_StackFrame> trace;
  for (uintptr_t i = 0; i < 20; ++i) {
    ddog_crasht_StackFrame frame = {.ip = i,
                                          .module_base_address = 0,
                                          .names = {.ptr = &names[i], .len = 1},
                                          .sp = 0,
                                          .symbol_address = 0};
    trace.push_back(frame);
  }
  ddog_crasht_Slice_StackFrame trace_slice = {.ptr = trace.data(), .len = trace.size()};

  check_result(ddog_crasht_CrashInfo_set_stacktrace(crashinfo.get(), to_slice_c_char(""), trace_slice),
               "Failed to set stacktrace");
}

int main(void) {
  auto crashinfo_new_result = ddog_crasht_CrashInfo_new();
  if (crashinfo_new_result.tag != DDOG_CRASHT_CRASH_INFO_NEW_RESULT_OK) {
    print_error("Failed to make new crashinfo: ", crashinfo_new_result.err);
    ddog_Error_drop(&crashinfo_new_result.err);
    exit(EXIT_FAILURE);
  }
  std::unique_ptr<ddog_crasht_CrashInfo, Deleter> crashinfo{&crashinfo_new_result.ok};

  check_result(
      ddog_crasht_CrashInfo_add_counter(crashinfo.get(), to_slice_c_char("my_amazing_counter"), 3),
      "Failed to add counter");

  // TODO add some tags here
  auto tags = ddog_Vec_Tag_new();
  const ddog_crasht_Metadata metadata = {
      .profiling_library_name = to_slice_c_char("libdatadog"),
      .profiling_library_version = to_slice_c_char("42"),
      .family = to_slice_c_char("rust"),
      .tags = &tags,
  };

  // TODO: We should set more tags that are expected by telemetry
  check_result(ddog_crasht_CrashInfo_set_metadata(crashinfo.get(), metadata), "Failed to add metadata");
  check_result(ddog_crasht_CrashInfo_add_tag(crashinfo.get(), to_slice_c_char("best hockey team"),
                                      to_slice_c_char("Habs")),
               "Failed to add tag");

  // This API allows one to capture useful files (e.g. /proc/pid/maps)
  // For testing purposes, use `/etc/hosts` which should exist on any reasonable
  // UNIX system
  check_result(ddog_crasht_CrashInfo_add_file(crashinfo.get(), to_slice_c_char("/etc/hosts")),
               "Failed to add file");

  add_stacktrace(crashinfo);

  // Datadog IPO at 2019-09-19T13:30:00Z = 1568899800 unix
  check_result(ddog_crasht_CrashInfo_set_timestamp(crashinfo.get(), 1568899800, 0),
               "Failed to set timestamp");

  auto endpoint = ddog_endpoint_from_filename(to_slice_c_char("/tmp/test"));

  check_result(ddog_crasht_CrashInfo_upload_to_endpoint(crashinfo.get(), endpoint),
               "Failed to export to file");
  ddog_endpoint_drop(endpoint);
}
