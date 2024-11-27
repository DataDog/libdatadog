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
static ddog_CharSlice to_slice_c_char(const char *s, std::size_t size) { return {.ptr = s, .len = size}; }
static ddog_CharSlice to_slice_string(std::string const &s) {
  return {.ptr = s.data(), .len = s.length()};
}

template <class T>
static ddog_ByteSlice to_byte_slice(T const& c) {
  return {.ptr = reinterpret_cast<const std::uint8_t*>(c.data()), .len = c.size()};
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
  constexpr std::size_t nb_elements = 20;
  std::vector<std::pair<std::string, std::string>> functions_and_filenames{nb_elements};
  for (uintptr_t i = 0; i < nb_elements; ++i) {
    functions_and_filenames.push_back({"func_" + std::to_string(i), "/path/to/code/file_" + std::to_string(i)});
  }

  std::vector<ddog_crasht_StackFrameNames> names{nb_elements};
  for (auto i = 0; i < nb_elements; i++) {
    auto const& [function_name, filename] = functions_and_filenames[i];

    auto function_name_slice = to_slice_string(function_name);
    auto res = ddog_crasht_demangle(function_name_slice, DDOG_CRASHT_DEMANGLE_OPTIONS_COMPLETE);
    if (res.tag == DDOG_CRASHT_STRING_WRAPPER_RESULT_OK)
    {
      auto string_result = res.ok.message;
      function_name_slice = to_slice_c_char((const char*)string_result.ptr, string_result.len);
    }

    names.push_back({.colno = ddog_Option_U32_some(i),
                     .filename = to_slice_string(filename),
                     .lineno = ddog_Option_U32_some(2 * i + 3),
                     .name = function_name_slice});
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

  std::vector<std::uint8_t> build_id = {42};
  std::string filePath = "/usr/share/somewhere";
  // test with normalized
  auto elfFrameWithNormalization = ddog_crasht_StackFrame{
    .ip = 42,
    .module_base_address = 0,
    .names = {.ptr = &names[0], .len = 1}, // just for the test
    .normalized_ip = {
      .file_offset = 1,
      .build_id = to_byte_slice(build_id),
      .path = to_slice_c_char(filePath.c_str(), filePath.size()),
      .typ = DDOG_CRASHT_NORMALIZED_ADDRESS_TYPES_ELF,
    },
    .sp = 0,
    .symbol_address = 0,
  };

  trace.push_back(elfFrameWithNormalization);

  // Windows-kind of frame
  auto dllFrameWithNormalization = ddog_crasht_StackFrame{
    .ip = 42,
    .module_base_address = 0,
    .names = {.ptr = &names[0], .len = 1}, // just for the test
    .normalized_ip = {
      .file_offset = 1,
      .build_id = to_byte_slice(build_id),
      .age = 21,
      .path = to_slice_c_char(filePath.c_str(), filePath.size()),
      .typ = DDOG_CRASHT_NORMALIZED_ADDRESS_TYPES_PDB,
    },
    .sp = 0,
    .symbol_address = 0,
  };
  
  trace.push_back(dllFrameWithNormalization);

  ddog_crasht_Slice_StackFrame trace_slice = {.ptr = trace.data(), .len = trace.size()};

  check_result(
      ddog_crasht_CrashInfo_set_stacktrace(crashinfo.get(), to_slice_c_char(""), trace_slice),
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
      .library_name = to_slice_c_char("libdatadog"),
      .library_version = to_slice_c_char("42"),
      .family = to_slice_c_char("rust"),
      .tags = &tags,
  };

  // TODO: We should set more tags that are expected by telemetry
  check_result(ddog_crasht_CrashInfo_set_metadata(crashinfo.get(), metadata),
               "Failed to add metadata");
  check_result(ddog_crasht_CrashInfo_add_tag(crashinfo.get(), to_slice_c_char("best hockey team"),
                                             to_slice_c_char("Habs")),
               "Failed to add tag");

  // This API allows one to capture useful files (e.g. /proc/pid/maps)
  // For testing purposes, use `/etc/hosts` which should exist on any reasonable
  // UNIX system
  check_result(ddog_crasht_CrashInfo_add_file(crashinfo.get(), to_slice_c_char("/etc/hosts")),
               "Failed to add file");

  add_stacktrace(crashinfo);

  ddog_Timespec timestamp = {.seconds = 1568899800, .nanoseconds = 0};
  // Datadog IPO at 2019-09-19T13:30:00Z = 1568899800 unix
  check_result(ddog_crasht_CrashInfo_set_timestamp(crashinfo.get(), timestamp),
               "Failed to set timestamp");

  ddog_crasht_ProcInfo procinfo = {
    .pid = 42
  };

  check_result(ddog_crasht_CrashInfo_set_procinfo(crashinfo.get(), procinfo),
               "Failed to set procinfo");

  auto endpoint = ddog_endpoint_from_filename(to_slice_c_char("/tmp/test"));

  check_result(ddog_crasht_CrashInfo_upload_to_endpoint(crashinfo.get(), endpoint),
               "Failed to export to file");
  ddog_endpoint_drop(endpoint);
}
