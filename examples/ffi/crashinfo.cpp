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

static std::string to_string(ddog_CharSlice s)
{
  return std::string(s.ptr, s.len);
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
    void operator()(ddog_crasht_Handle_##typ *object) {                                            \
      ddog_crasht_##typ##_drop(object);                                                            \
      delete object;                                                                               \
    }                                                                                              \
  };                                                                                               \
  std::unique_ptr<ddog_crasht_Handle_##typ, typ##Deleter> extract_result(                          \
      ddog_crasht_##typ##_NewResult result, const char *msg) {                                    \
    if (result.tag != ok_tag) {                                                                    \
      print_error(msg, result.err);                                                                \
      ddog_Error_drop(&result.err);                                                                \
      exit(EXIT_FAILURE);                                                                          \
    }                                                                                              \
    std::unique_ptr<ddog_crasht_Handle_##typ, typ##Deleter> rval{                                  \
        new ddog_crasht_Handle_##typ{result.ok}};                                                  \
    return rval;                                                                                   \
  }

EXTRACT_RESULT(CrashInfoBuilder,
               DDOG_CRASHT_CRASH_INFO_BUILDER_NEW_RESULT_OK)
EXTRACT_RESULT(CrashInfo, DDOG_CRASHT_CRASH_INFO_NEW_RESULT_OK)
EXTRACT_RESULT(StackTrace, DDOG_CRASHT_STACK_TRACE_NEW_RESULT_OK)
EXTRACT_RESULT(StackFrame, DDOG_CRASHT_STACK_FRAME_NEW_RESULT_OK)

std::optional<std::string> demangle(std::string const& name)
{
  // We must keep this call to demangle to check that the symbol is exported.
  auto result = ddog_crasht_demangle(to_slice_string(name), DDOG_CRASHT_DEMANGLE_OPTIONS_COMPLETE);
  if (result.tag == DDOG_STRING_WRAPPER_RESULT_OK)
  {
    auto demangled_name = to_string(ddog_StringWrapper_message(&result.ok));
    ddog_StringWrapper_drop(&result.ok);
    return demangled_name;
  }

  print_error("Failed to demangle string", result.err);
  ddog_Error_drop(&result.err);
  return {};
}

void add_random_frames(ddog_crasht_Handle_StackTrace* stacktrace) {
  for (uintptr_t i = 0; i < 10; ++i) {
    auto new_frame = extract_result(ddog_crasht_StackFrame_new(), "failed to make StackFrame");
    std::string name = "func_" + std::to_string(i);
    auto function_name = demangle(name).value_or(name);

    check_result(ddog_crasht_StackFrame_with_function(new_frame.get(), to_slice_string(function_name)),
                 "failed to add function");
    std::string filename = "/path/to/code/file_" + std::to_string(i);
    check_result(ddog_crasht_StackFrame_with_file(new_frame.get(), to_slice_string(filename)),
                 "failed to add filename");
    check_result(ddog_crasht_StackFrame_with_line(new_frame.get(), i * 4 + 3),
                 "failed to add line");
    check_result(ddog_crasht_StackFrame_with_column(new_frame.get(), i * 3 + 7),
                 "failed to add line");

    // This operation consumes the frame, so use .release here
    check_result(ddog_crasht_StackTrace_push_frame(stacktrace, new_frame.release(), true),
                 "failed to add stack frame");
  }

}

void add_windows_style_frame(ddog_crasht_Handle_StackTrace* stacktrace) {
  // Windows style frame with normalization
  auto pbd_frame = extract_result(ddog_crasht_StackFrame_new(), "failed to make StackFrame");
  check_result(ddog_crasht_StackFrame_with_ip(pbd_frame.get(), 0XDEADBEEF),
               "failed to add ip");
  check_result(ddog_crasht_StackFrame_with_module_base_address(pbd_frame.get(), 0XABBAABBA),
               "failed to add module_base_address");
  check_result(
      ddog_crasht_StackFrame_with_build_id(pbd_frame.get(), to_slice_c_char("abcdef12345")),
      "failed to add build id");
  check_result(
      ddog_crasht_StackFrame_with_build_id_type(pbd_frame.get(), DDOG_CRASHT_BUILD_ID_TYPE_PDB),
      "failed to add build id type");
  check_result(ddog_crasht_StackFrame_with_file_type(pbd_frame.get(), DDOG_CRASHT_FILE_TYPE_PE),
               "failed to add file type");
  check_result(ddog_crasht_StackFrame_with_path(
                   pbd_frame.get(), to_slice_c_char("C:/Program Files/best_program_ever.exe")),
               "failed to add path");
  check_result(
      ddog_crasht_StackFrame_with_relative_address(pbd_frame.get(), 0XBABEF00D),
      "failed to add relative address");
  // This operation consumes the frame, so use .release here
  check_result(ddog_crasht_StackTrace_push_frame(stacktrace, pbd_frame.release(), true),
               "failed to add stack frame");
}

void add_elf_frame(ddog_crasht_Handle_StackTrace* stacktrace) {
  // ELF style frame with normalization
  auto elf_frame = extract_result(ddog_crasht_StackFrame_new(), "failed to make StackFrame");
  check_result(ddog_crasht_StackFrame_with_ip(elf_frame.get(), 0XDEADBEEF),
               "failed to add ip");
  check_result(ddog_crasht_StackFrame_with_module_base_address(elf_frame.get(), 0XABBAABBA),
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
      ddog_crasht_StackFrame_with_relative_address(elf_frame.get(), 0XBABEF00D),
      "failed to add relative address");
  // This operation consumes the frame, so use .release here
  check_result(ddog_crasht_StackTrace_push_frame(stacktrace, elf_frame.release(), true),
               "failed to add stack frame");
}

void add_thread(ddog_crasht_Handle_CrashInfoBuilder *builder) {
  auto stacktrace = extract_result(ddog_crasht_StackTrace_new(), "failed to make new StackTrace");
  add_random_frames(stacktrace.get());

  add_windows_style_frame(stacktrace.get());

  add_elf_frame(stacktrace.get());

  auto thread = ddog_crasht_ThreadData{
    .crashed = false,
    .name = to_slice_c_char("main thread"),
    .stack = *stacktrace.release(), // stacktrace is consumed so use release
    .state = to_slice_c_char("sleeping")
  };
  check_result(ddog_crasht_CrashInfoBuilder_with_thread(builder, thread), "failed to add a thread");
}

void add_stacktrace(ddog_crasht_Handle_CrashInfoBuilder *builder) {
  auto stacktrace = extract_result(ddog_crasht_StackTrace_new(), "failed to make new StackTrace");

  add_random_frames(stacktrace.get());

  add_windows_style_frame(stacktrace.get());

  add_elf_frame(stacktrace.get());

  check_result(ddog_crasht_StackTrace_set_complete(stacktrace.get()),
               "unable to set stacktrace as complete");

  // Now that all the frames are added to the stack, put the stack on the report
  // This operation consumes the stack, so use .release here
  check_result(ddog_crasht_CrashInfoBuilder_with_stack(builder, stacktrace.release()),
               "failed to add stacktrace");

  add_thread(builder);
}

int main(void) {
  auto builder = extract_result(ddog_crasht_CrashInfoBuilder_new(), "failed to make builder");
  check_result(ddog_crasht_CrashInfoBuilder_with_counter(builder.get(),
                                                         to_slice_c_char("my_amazing_counter"), 3),
               "Failed to add counter");

  auto tags = ddog_Vec_Tag_new();
  check_result(
      ddog_Vec_Tag_push(&tags, to_slice_c_char("best-hockey-team"), to_slice_c_char("Habs")),
      "failed to add tag");
  const ddog_crasht_Metadata metadata = {
      .library_name = to_slice_c_char("libdatadog"),
      .library_version = to_slice_c_char("42"),
      .family = to_slice_c_char("rust"),
      .tags = &tags,
  };

  check_result(ddog_crasht_CrashInfoBuilder_with_metadata(builder.get(), metadata),
               "Failed to add metadata");
  ddog_Vec_Tag_drop(tags);

  // This API allows one to capture useful files (e.g. /proc/pid/maps)
  // For testing purposes, use `/etc/hosts` which should exist on any reasonable UNIX system
  check_result(ddog_crasht_CrashInfoBuilder_with_file(builder.get(), to_slice_c_char("/etc/hosts")),
               "Failed to add file");

  check_result(ddog_crasht_CrashInfoBuilder_with_kind(builder.get(), DDOG_CRASHT_ERROR_KIND_PANIC),
               "Failed to set error kind");

  add_stacktrace(builder.get());

  // Datadog IPO at 2019-09-19T13:30:00Z = 1568899800 unix
  ddog_Timespec timestamp = {.seconds = 1568899800, .nanoseconds = 0};
  check_result(ddog_crasht_CrashInfoBuilder_with_timestamp(builder.get(), timestamp),
               "Failed to set timestamp");

  ddog_crasht_ProcInfo procinfo = {.pid = 42};
  check_result(ddog_crasht_CrashInfoBuilder_with_proc_info(builder.get(), procinfo),
               "Failed to set procinfo");

  check_result(ddog_crasht_CrashInfoBuilder_with_os_info_this_machine(builder.get()),
               "Failed to set os_info");

  // Test uploading a crash ping without siginfo
  auto ping_endpoint = ddog_endpoint_from_filename(to_slice_c_char("/tmp/crash_ping_test"));
  check_result(ddog_crasht_CrashInfoBuilder_upload_ping_to_endpoint(builder.get(), ping_endpoint),
               "Failed to upload crash ping");
  ddog_endpoint_drop(ping_endpoint);

  auto sigInfo = ddog_crasht_SigInfo {
    .addr = "0xBABEF00D",
    .code = 16,
    .code_human_readable = DDOG_CRASHT_SI_CODES_UNKNOWN,
    .signo = -1,
    .signo_human_readable = DDOG_CRASHT_SIGNAL_NAMES_UNKNOWN
  };

  check_result(ddog_crasht_CrashInfoBuilder_with_sig_info(builder.get(), sigInfo),
               "failed to add signal info");

  auto ping_endpoint2 = ddog_endpoint_from_filename(to_slice_c_char("/tmp/crash_ping_test"));
  check_result(ddog_crasht_CrashInfoBuilder_upload_ping_to_endpoint(builder.get(), ping_endpoint2),
               "Failed to upload crash ping");
  ddog_endpoint_drop(ping_endpoint2);

  auto crashinfo = extract_result(ddog_crasht_CrashInfoBuilder_build(builder.release()),
                                  "failed to build CrashInfo");
  auto endpoint = ddog_endpoint_from_filename(to_slice_c_char("/tmp/test"));
  check_result(ddog_crasht_CrashInfo_upload_to_endpoint(crashinfo.get(), endpoint),
               "Failed to export to file");
  ddog_endpoint_drop(endpoint);
}
