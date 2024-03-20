// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

extern "C" {
#include <datadog/common.h>
#include <datadog/profiling.h>
}
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <thread>

static ddog_CharSlice to_slice_c_char(const char *s) { return {.ptr = s, .len = strlen(s)}; }
static ddog_CharSlice to_slice_string(std::string &s) {
  return {.ptr = s.data(), .len = s.length()};
}

// TODO: Testing on my mac, the tags appear to have the opposite meaning you'd
// expect
static ddog_prof_Option_U32 some_u32(uint32_t i) {
  ddog_prof_Option_U32 rval;
  rval.some = i;
  rval.tag = DDOG_PROF_OPTION_U32_NONE_U32;
  return rval;
}
static ddog_prof_Option_U32 none_u32() { return {.tag = DDOG_PROF_OPTION_U32_SOME_U32}; }

struct Deleter {
  void operator()(ddog_prof_CrashInfo *object) { ddog_crashinfo_drop(object); }
};

void print_error(const char *s, const ddog_Error &err) {
  auto charslice = ddog_Error_message(&err);
  printf("%s (%.*s)\n", s, static_cast<int>(charslice.len), charslice.ptr);
}

void check_result(ddog_prof_CrashtrackerResult result, const char *msg) {
  if (result.tag != DDOG_PROF_CRASHTRACKER_RESULT_OK) {
    print_error(msg, result.err);
    ddog_Error_drop(&result.err);
    exit(EXIT_FAILURE);
  }
}

void add_stacktrace(std::unique_ptr<ddog_prof_CrashInfo, Deleter> &crashinfo) {

  // Collect things into vectors so they stay alive till the function exits
  std::vector<std::string> filenames;
  std::vector<std::string> function_names;
  for (uintptr_t i = 0; i < 20; ++i) {
    filenames.push_back("/path/to/code/file_" + std::to_string(i));
    function_names.push_back("func_" + std::to_string(i));
  }

  std::vector<ddog_prof_StackFrameNames> names;
  for (uintptr_t i = 0; i < 20; ++i) {
    names.push_back({.colno = some_u32(i),
                     .filename = to_slice_string(filenames[i]),
                     .lineno = some_u32(2 * i + 3),
                     .name = to_slice_string(function_names[i])});
  }

  std::vector<ddog_prof_StackFrame> trace;
  for (uintptr_t i = 0; i < 20; ++i) {
    ddog_prof_StackFrame frame = {.ip = i,
                                  .module_base_address = 0,
                                  .names = {.ptr = &names[i], .len = 1},
                                  .sp = 0,
                                  .symbol_address = 0};
    trace.push_back(frame);
  }
  ddog_prof_Slice_StackFrame trace_slice = {.ptr = trace.data(), .len = trace.size()};

  check_result(ddog_crashinfo_set_stacktrace(crashinfo.get(), to_slice_c_char(""), trace_slice),
               "Failed to set stacktrace");
}

int main(void) {
  auto crashinfo_new_result = ddog_crashinfo_new();
  if (crashinfo_new_result.tag != DDOG_PROF_CRASH_INFO_NEW_RESULT_OK) {
    print_error("Failed to make new crashinfo: ", crashinfo_new_result.err);
    ddog_Error_drop(&crashinfo_new_result.err);
    exit(EXIT_FAILURE);
  }
  std::unique_ptr<ddog_prof_CrashInfo, Deleter> crashinfo{&crashinfo_new_result.ok};

  check_result(
      ddog_crashinfo_add_counter(crashinfo.get(), to_slice_c_char("my_amazing_counter"), 3),
      "Failed to add counter");

  // TODO add some tags here
  auto tags = ddog_Vec_Tag_new();
  const ddog_prof_CrashtrackerMetadata metadata = {
      .profiling_library_name = to_slice_c_char("libdatadog"),
      .profiling_library_version = to_slice_c_char("42"),
      .family = to_slice_c_char("rust"),
      .tags = &tags,
  };

  check_result(ddog_crashinfo_set_metadata(crashinfo.get(), metadata), "Failed to add metadata");

  // This API allows one to capture useful files (e.g. /proc/pid/maps)
  // For testing purposes, use `/etc/hosts` which should exist on any reasonable
  // UNIX system
  check_result(ddog_crashinfo_add_file(crashinfo.get(), to_slice_c_char("/etc/hosts")),
               "Failed to add file");

  add_stacktrace(crashinfo);

  auto endpoint = ddog_Endpoint_file(to_slice_c_char("file://tmp/test.txt"));
  check_result(ddog_crashinfo_upload_to_endpoint(crashinfo.get(), endpoint, 1),
               "Failed to export to file");
  ddog_prof_Option_U32 opt;
}