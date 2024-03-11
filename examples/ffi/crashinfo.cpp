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
  ddog_Endpoint endpoint = ddog_Endpoint_agent(DDOG_CHARSLICE_C("file://./test.txt"));
  check_result(ddog_crashinfo_upload_to_endpoint(crashinfo.get(), endpoint, 1),
               "Failed to export to file");
}