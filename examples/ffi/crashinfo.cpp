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
static ddog_CharSlice to_slice_c_char(const char *s, std::size_t size) {
  return {.ptr = s, .len = size};
}
static ddog_CharSlice to_slice_string(std::string const &s) {
  return {.ptr = s.data(), .len = s.length()};
}

void print_error(const char *s, const ddog_Error &err) {
  auto charslice = ddog_Error_message(&err);
  printf("%s (%.*s)\n", s, static_cast<int>(charslice.len), charslice.ptr);
}

#define CHECK_RESULT(typ, ok_tag)                                                                  \
  void check_result(typ result, const char *msg) {                                                 \
    if (result.tag != ok_tag) {                                                                    \
      print_error(msg, result.err);                                                                \
      ddog_Error_drop(&result.err);                                                                \
      exit(EXIT_FAILURE);                                                                          \
    }                                                                                              \
  }

CHECK_RESULT(ddog_VoidResult, DDOG_VOID_RESULT_OK)
CHECK_RESULT(ddog_Vec_Tag_PushResult, DDOG_VEC_TAG_PUSH_RESULT_OK)

#define EXTRACT_RESULT(typ, ok_tag)                                                                \
  struct typ##Deleter {                                                                            \
    void operator()(ddog_crasht_Handle_##typ *object) { ddog_crasht_##typ##_drop(object); }        \
  };                                                                                               \
  std::unique_ptr<ddog_crasht_Handle_##typ, typ##Deleter> extract_result(                          \
      ddog_crasht_Result_Handle##typ result, const char *msg) {                                    \
    if (result.tag != ok_tag) {                                                                    \
      print_error(msg, result.err);                                                                \
      ddog_Error_drop(&result.err);                                                                \
      exit(EXIT_FAILURE);                                                                          \
    }                                                                                              \
    std::unique_ptr<ddog_crasht_Handle_##typ, typ##Deleter> rval{&result.ok};                      \
    return rval;                                                                                   \
  }

EXTRACT_RESULT(CrashInfoBuilder,
               DDOG_CRASHT_RESULT_HANDLE_CRASH_INFO_BUILDER_OK_HANDLE_CRASH_INFO_BUILDER)
EXTRACT_RESULT(CrashInfo, DDOG_CRASHT_RESULT_HANDLE_CRASH_INFO_OK_HANDLE_CRASH_INFO)
EXTRACT_RESULT(StackTrace, DDOG_CRASHT_RESULT_HANDLE_STACK_TRACE_OK_HANDLE_STACK_TRACE)
EXTRACT_RESULT(StackFrame, DDOG_CRASHT_RESULT_HANDLE_STACK_FRAME_OK_HANDLE_STACK_FRAME)

void add_stacktrace(ddog_crasht_Handle_CrashInfoBuilder *builder) {
  auto stacktrace = extract_result(ddog_crasht_StackTrace_new(), "failed to make new StackTrace");

  for (uintptr_t i = 0; i < 10; ++i) {
    auto new_frame = extract_result(ddog_crasht_StackFrame_new(), "failed to make StackFrame");
    std::string name = "func_" + std::to_string(i);
    check_result(ddog_crasht_StackFrame_with_function(new_frame.get(), to_slice_string(name)),
                 "failed to add function");
    std::string filename = "/path/to/code/file_" + std::to_string(i);
    check_result(ddog_crasht_StackFrame_with_file(new_frame.get(), to_slice_string(filename)),
                 "failed to add filename");
    check_result(ddog_crasht_StackFrame_with_line(new_frame.get(), i * 4 + 3),
                 "failed to add line");
    check_result(ddog_crasht_StackFrame_with_column(new_frame.get(), i * 3 + 7),
                 "failed to add line");

    // This operation consumes the frame, so use .release here
    check_result(ddog_crasht_StackTrace_push_frame(stacktrace.get(), new_frame.release()),
                 "failed to add stack frame");
  }

  // Windows style frame with normalization
  auto pbd_frame = extract_result(ddog_crasht_StackFrame_new(), "failed to make StackFrame");
  check_result(ddog_crasht_StackFrame_with_ip(pbd_frame.get(), to_slice_c_char("0xDEADBEEF")),
               "failed to add ip");
  check_result(ddog_crasht_StackFrame_with_module_base_address(pbd_frame.get(),
                                                               to_slice_c_char("0xABBAABBA")),
               "failed to add module_base_address");
  check_result(
      ddog_crasht_StackFrame_with_build_id(pbd_frame.get(), to_slice_c_char("abcdef12345")),
      "failed to add build id");
  check_result(
      ddog_crasht_StackFrame_with_build_id_type(pbd_frame.get(), DDOG_CRASHT_BUILD_ID_TYPE_PDB),
      "failed to add build id type");
  check_result(ddog_crasht_StackFrame_with_file_type(pbd_frame.get(), DDOG_CRASHT_FILE_TYPE_PDB),
               "failed to add file type");
  check_result(ddog_crasht_StackFrame_with_path(
                   pbd_frame.get(), to_slice_c_char("C:/Program Files/best_program_ever.exe")),
               "failed to add path");
  check_result(
      ddog_crasht_StackFrame_with_relative_address(pbd_frame.get(), to_slice_c_char("0xBABEF00D")),
      "failed to add relative address");
  // This operation consumes the frame, so use .release here
  check_result(ddog_crasht_StackTrace_push_frame(stacktrace.get(), pbd_frame.release()),
               "failed to add stack frame");

  // ELF style frame with normalization
  auto elf_frame = extract_result(ddog_crasht_StackFrame_new(), "failed to make StackFrame");
  check_result(ddog_crasht_StackFrame_with_ip(elf_frame.get(), to_slice_c_char("0xDEADBEEF")),
               "failed to add ip");
  check_result(ddog_crasht_StackFrame_with_module_base_address(elf_frame.get(),
                                                               to_slice_c_char("0xABBAABBA")),
               "failed to add module_base_address");
  check_result(
      ddog_crasht_StackFrame_with_build_id(elf_frame.get(), to_slice_c_char("987654321fedcba0")),
      "failed to add build id");
  check_result(
      ddog_crasht_StackFrame_with_build_id_type(elf_frame.get(), DDOG_CRASHT_BUILD_ID_TYPE_GNU),
      "failed to add build id type");
  check_result(ddog_crasht_StackFrame_with_file_type(elf_frame.get(), DDOG_CRASHT_FILE_TYPE_ELF),
               "failed to add file type");
  check_result(ddog_crasht_StackFrame_with_path(elf_frame.get(),
                                                to_slice_c_char("/usr/bin/awesome-gnu-utility.so")),
               "failed to add path");
  check_result(
      ddog_crasht_StackFrame_with_relative_address(elf_frame.get(), to_slice_c_char("0xBABEF00D")),
      "failed to add relative address");
  // This operation consumes the frame, so use .release here
  check_result(ddog_crasht_StackTrace_push_frame(stacktrace.get(), elf_frame.release()),
               "failed to add stack frame");

  // Now that all the frames are added to the stack, put the stack on the report
  // This operation consumes the stack, so use .release here
  check_result(ddog_crasht_CrashInfoBuilder_with_stack(builder, stacktrace.release()),
               "failed to add stacktrace");
}

int main(void) {
  auto builder = extract_result(ddog_crasht_CrashInfoBuilder_new(), "failed to make builder");
  // check_result(ddog_crasht_CrashInfoBuilder_with_counter(builder.get(),
  //                                                        to_slice_c_char("my_amazing_counter"), 3),
  //              "Failed to add counter");

  // auto tags = ddog_Vec_Tag_new();
  // check_result(
  //     ddog_Vec_Tag_push(&tags, to_slice_c_char("best-hockey-team"), to_slice_c_char("Habs")),
  //     "failed to add tag");
  // const ddog_crasht_Metadata metadata = {
  //     .library_name = to_slice_c_char("libdatadog"),
  //     .library_version = to_slice_c_char("42"),
  //     .family = to_slice_c_char("rust"),
  //     .tags = &tags,
  // };

  // check_result(ddog_crasht_CrashInfoBuilder_with_metadata(builder.get(), metadata),
  //              "Failed to add metadata");
  // ddog_Vec_Tag_drop(tags);

  // // This API allows one to capture useful files (e.g. /proc/pid/maps)
  // // For testing purposes, use `/etc/hosts` which should exist on any reasonable UNIX system
  // check_result(ddog_crasht_CrashInfoBuilder_with_file(builder.get(), to_slice_c_char("/etc/hosts")),
  //              "Failed to add file");

  // check_result(ddog_crasht_CrashInfoBuilder_with_kind(builder.get(), DDOG_CRASHT_ERROR_KIND_PANIC),
  //              "Failed to set error kind");

  // add_stacktrace(builder.get());

  // // Datadog IPO at 2019-09-19T13:30:00Z = 1568899800 unix
  // ddog_Timespec timestamp = {.seconds = 1568899800, .nanoseconds = 0};
  // check_result(ddog_crasht_CrashInfoBuilder_with_timestamp(builder.get(), timestamp),
  //              "Failed to set timestamp");

  // ddog_crasht_ProcInfo procinfo = {.pid = 42};
  // check_result(ddog_crasht_CrashInfoBuilder_with_proc_info(builder.get(), procinfo),
  //              "Failed to set procinfo");

  // check_result(ddog_crasht_CrashInfoBuilder_with_os_info_this_machine(builder.get()),
  //              "Failed to set os_info");

  auto crashinfo = extract_result(ddog_crasht_CrashInfoBuilder_build(builder.release()),
                                  "failed to build CrashInfo");
  auto endpoint = ddog_endpoint_from_filename(to_slice_c_char("/tmp/test"));
  check_result(ddog_crasht_CrashInfo_upload_to_endpoint(crashinfo.get(), endpoint),
               "Failed to export to file");
  ddog_endpoint_drop(endpoint);
}
