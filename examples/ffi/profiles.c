// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/common.h>
#include <datadog/profiling.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#endif

/* This example simulates gathering wall and allocation samples. Roughly every
 * minute it will export the data, assumes an http://localhost:8126/ location
 * for the agent.
 * This adds DD_SERVICE, DD_ENV, and DD_VERSION version as tags.
 */

static const int EXPORT_INTERVAL = 10; // seconds

static void check_ok(struct ddog_prof_Status status, const char *context) {
  if (status.flags != 0) {
    fprintf(stderr, "%s: %s\n", context, status.err);
    ddog_prof_Status_drop(&status);
    // this will cause leaks but this is just an example.
    exit(EXIT_FAILURE);
  }
}

static ddog_CharSlice to_slice_c_char(const char *s) {
  return (ddog_CharSlice){.ptr = s, .len = s ? strlen(s) : 0};
}

static volatile sig_atomic_t should_continue = 1;
static void sighandler(int signo) {
  (void)signo;
  should_continue = 0;
}

static void add_unified_service_tag(struct ddog_Vec_Tag *tags, const char *key, const char *value) {
  if (value && value[0]) {
    struct ddog_Vec_Tag_PushResult r =
        ddog_Vec_Tag_push(tags, to_slice_c_char(key), to_slice_c_char(value));
    if (r.tag != DDOG_VEC_TAG_PUSH_RESULT_OK) {
      ddog_CharSlice message = ddog_Error_message(&r.err);
      fprintf(stderr, "Failed to push tag %s: %.*s\n", key, (int)message.len, message.ptr);
      ddog_Error_drop(&r.err);
      // Leak and exit for simplicity in example
      exit(EXIT_FAILURE);
    }
  }
}

static struct ddog_Timespec now_wall(void) {
  struct timespec ts;
#ifdef CLOCK_REALTIME
  clock_gettime(CLOCK_REALTIME, &ts);
#else
  // Fallback: coarse but fine for example
  ts.tv_sec = time(NULL);
  ts.tv_nsec = 0;
#endif
  struct ddog_Timespec dd = {.seconds = (int64_t)ts.tv_sec, .nanoseconds = (uint32_t)ts.tv_nsec};
  return dd;
}

// Cross-platform sleep with millisecond precision (sufficient for ~10ms accuracy)
static void sleep_ms(unsigned int ms) {
#ifdef _WIN32
  Sleep(ms);
#else
  struct timespec req;
  req.tv_sec = ms / 1000;
  req.tv_nsec = (long)((ms % 1000) * 1000000L);
  nanosleep(&req, NULL);
#endif
}

static int64_t rand_range_i64(int64_t min_inclusive, int64_t max_inclusive) {
  if (max_inclusive <= min_inclusive)
    return min_inclusive;
  uint64_t span = (uint64_t)(max_inclusive - min_inclusive + 1);
  return (int64_t)(min_inclusive + (rand() % span));
}

static void rebuild_scratchpad_state(ddog_prof_ScratchPadHandle pad, ddog_prof_FunctionId func_id,
                                     ddog_prof_MappingId map_id, ddog_prof_StackId *out_stack_id) {
  ddog_prof_Line line = {.function_id = func_id};
  ddog_prof_Location loc = {.mapping_id = map_id, .line = line};
  ddog_prof_LocationId locs[1];
  check_ok(ddog_prof_ScratchPad_insert_location(locs, pad, &loc), "ScratchPad_insert_location");
  ddog_prof_Slice_LocationId loc_slice = {.ptr = locs, .len = 1};
  out_stack_id->thin_ptr = NULL;
  check_ok(ddog_prof_ScratchPad_insert_stack(out_stack_id, pad, loc_slice),
           "ScratchPad_insert_stack");
}

int main(void) {
  // Seed randomness for simulated allocation sizes
  srand((unsigned)time(NULL));

  // Create core handles
  ddog_prof_ProfilesDictionaryHandle dict = NULL;
  check_ok(ddog_prof_ProfilesDictionary_new(&dict), "ProfilesDictionary_new");

  ddog_prof_ScratchPadHandle scratch = NULL;
  check_ok(ddog_prof_ScratchPad_new(&scratch), "ScratchPad_new");

  // Insert function/mapping strings and create ids
  ddog_prof_Function func = {.system_name = DDOG_PROF_STRINGID_EMPTY};
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&func.name, dict, DDOG_CHARSLICE_C("{main}"),
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(fn name)");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&func.file_name, dict,
                                                   DDOG_CHARSLICE_C("/srv/example/index.php"),
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(fn file)");
  ddog_prof_FunctionId func_id = NULL;
  check_ok(ddog_prof_ProfilesDictionary_insert_function(&func_id, dict, &func), "insert_function");

  ddog_prof_Mapping mapping = {.build_id = DDOG_PROF_STRINGID_EMPTY};
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&mapping.filename, dict,
                                                   DDOG_CHARSLICE_C("/bin/example"),
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(map filename)");
  ddog_prof_MappingId map_id = NULL;
  check_ok(ddog_prof_ProfilesDictionary_insert_mapping(&map_id, dict, &mapping), "insert_mapping");

  // Prepare StringIds and ValueTypes for two profiles via ProfileAdapter:
  // - grouping 0: wall-time (nanoseconds)
  // - grouping 1: allocation profile with two sample types: bytes and count
  ddog_prof_ValueType wall_time_vt = {DDOG_PROF_STRINGID_EMPTY, DDOG_PROF_STRINGID_EMPTY};
  ddog_prof_ValueType alloc_space_vt = {DDOG_PROF_STRINGID_EMPTY, DDOG_PROF_STRINGID_EMPTY};
  ddog_prof_ValueType alloc_samples_vt = {DDOG_PROF_STRINGID_EMPTY, DDOG_PROF_STRINGID_EMPTY};
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&wall_time_vt.type_id, dict,
                                                   DDOG_CHARSLICE_C("wall-time"),
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(wall type)");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&wall_time_vt.unit_id, dict,
                                                   DDOG_CHARSLICE_C("nanoseconds"),
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(wall unit)");

  check_ok(ddog_prof_ProfilesDictionary_insert_str(&alloc_space_vt.type_id, dict,
                                                   DDOG_CHARSLICE_C("alloc-space"),
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(alloc bytes type)");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&alloc_space_vt.unit_id, dict,
                                                   DDOG_CHARSLICE_C("bytes"),
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(alloc bytes unit)");

  check_ok(ddog_prof_ProfilesDictionary_insert_str(&alloc_samples_vt.type_id, dict,
                                                   DDOG_CHARSLICE_C("alloc-samples"),
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(alloc count type)");
  check_ok(ddog_prof_ProfilesDictionary_insert_str(&alloc_samples_vt.unit_id, dict,
                                                   DDOG_CHARSLICE_C("count"),
                                                   DDOG_PROF_UTF8_OPTION_VALIDATE),
           "insert_str(alloc count unit)");

  ddog_prof_ValueType value_types[3] = {wall_time_vt, alloc_space_vt, alloc_samples_vt};
  int64_t groupings[3] = {0, 1, 1};

  // Initial per-interval setup
  ddog_prof_StackId stack_id = {0};
  rebuild_scratchpad_state(scratch, func_id, map_id, &stack_id);

  ddog_prof_ProfileAdapter adapter;
  check_ok(
      ddog_prof_ProfileAdapter_new(&adapter, dict, scratch,
                                   (struct ddog_prof_Slice_ValueType){.ptr = value_types, .len = 3},
                                   (struct ddog_Slice_I64){.ptr = groupings, .len = 3}),
      "ProfileAdapter_new");

  // Simulate Poisson upscaling for the allocation grouping (group index 1)
  const uint64_t SAMPLING_DISTANCE = 512 * 1024; // 512 KiB sampling distance
  struct ddog_prof_PoissonUpscalingRule poisson_rule = {
      .sum_offset = 0, .count_offset = 1, .sampling_distance = SAMPLING_DISTANCE};

  // These three tags are called unified service tags.
  // We treat empty and unset the same in this example.
  struct ddog_Vec_Tag tags = ddog_Vec_Tag_new();
  const char *tag_service = getenv("DD_SERVICE");
  const char *tag_env = getenv("DD_ENV");
  const char *tag_version = getenv("DD_VERSION");
  add_unified_service_tag(&tags, "service", tag_service);
  add_unified_service_tag(&tags, "env", tag_env);
  add_unified_service_tag(&tags, "version", tag_version);

  printf("[profiles.c] starting: tick=10ms, export_interval=%ds, endpoint=http://localhost:8126, "
         "service=%s, env=%s, version=%s\n",
         EXPORT_INTERVAL, (tag_service && tag_service[0]) ? tag_service : "(unset)",
         (tag_env && tag_env[0]) ? tag_env : "(unset)",
         (tag_version && tag_version[0]) ? tag_version : "(unset)");

  // Sampling loop: every 10ms; flush/export every EXPORT_INTERVAL seconds
  const int64_t WALL_TICK_NS = 10 * 1000 * 1000; // 10ms
  time_t interval_started = time(NULL);

  signal(SIGINT, sighandler);
  signal(SIGTERM, sighandler);
  while (should_continue) {
    struct ddog_Timespec ts_now = now_wall();

    // Sample 1: wall-time (group 0)
    {
      int64_t values[3] = {WALL_TICK_NS, 0, 0};
      ddog_prof_SampleBuilderHandle sb = NULL;
      check_ok(ddog_prof_ProfileAdapter_add_sample(
                   &sb, &adapter, 0, (struct ddog_Slice_I64){.ptr = values, .len = 3}),
               "ProfileAdapter_add_sample(wall)");
      check_ok(ddog_prof_SampleBuilder_stack_id(sb, stack_id), "SampleBuilder_stack_id(wall)");
      check_ok(ddog_prof_SampleBuilder_timestamp(sb, ts_now), "SampleBuilder_timestamp(wall)");
      check_ok(ddog_prof_SampleBuilder_finish(&sb), "SampleBuilder_finish(wall)");
    }

    // Sample 2: allocation (group 1) using Poisson upscaling
    {
      int64_t bytes = rand_range_i64(1, (int64_t)SAMPLING_DISTANCE);
      int64_t values[3] = {0, bytes, 1};
      ddog_prof_SampleBuilderHandle sb = NULL;
      check_ok(ddog_prof_ProfileAdapter_add_sample(
                   &sb, &adapter, 1, (struct ddog_Slice_I64){.ptr = values, .len = 3}),
               "ProfileAdapter_add_sample(alloc)");
      check_ok(ddog_prof_SampleBuilder_stack_id(sb, stack_id), "SampleBuilder_stack_id(alloc)");
      check_ok(ddog_prof_SampleBuilder_timestamp(sb, ts_now), "SampleBuilder_timestamp(alloc)");
      check_ok(ddog_prof_SampleBuilder_finish(&sb), "SampleBuilder_finish(alloc)");
    }

    // Flush once per interval
    time_t now_secs = time(NULL);
    if (now_secs - interval_started >= EXPORT_INTERVAL) {
      printf("[profiles.c] interval elapsed (%ds); exporting profiles...\n", EXPORT_INTERVAL);
      struct ddog_Timespec end_ts = now_wall();
      struct ddog_Timespec start_ts = {.seconds = (int64_t)interval_started, .nanoseconds = 0};
      struct ddog_prof_EncodedProfile encoded = {0};
      // Grouping 0: profile has no upscaling, no special API to call.
      // Grouping 1: add profile with poisson upscaling
      check_ok(ddog_prof_ProfileAdapter_add_poisson_upscaling(&adapter, 1, poisson_rule),
               "ProfileAdapter_add_poisson_upscaling)");
      check_ok(ddog_prof_ProfileAdapter_build_compressed(&encoded, &adapter, &start_ts, &end_ts),
               "PprofAdapter_build_compressed");

      // Build and send exporter request
      struct ddog_prof_Slice_Exporter_File files_to_compress =
          ddog_prof_Exporter_Slice_File_empty();
      struct ddog_prof_Slice_Exporter_File files_unmodified = ddog_prof_Exporter_Slice_File_empty();
      struct ddog_prof_Endpoint endpoint =
          ddog_prof_Endpoint_agent(DDOG_CHARSLICE_C("http://localhost:8126"));
      struct ddog_prof_ProfileExporter_Result exporter_result =
          ddog_prof_Exporter_new(DDOG_CHARSLICE_C("ffi-example"), DDOG_CHARSLICE_C("0.0.0"),
                                 DDOG_CHARSLICE_C("php"), &tags, endpoint);
      if (exporter_result.tag != DDOG_PROF_PROFILE_EXPORTER_RESULT_OK_HANDLE_PROFILE_EXPORTER) {
        ddog_CharSlice message = ddog_Error_message(&exporter_result.err);
        fprintf(stderr, "Failed to create exporter: %.*s\n", (int)message.len, message.ptr);
        ddog_Error_drop(&exporter_result.err);
        break;
      }
      struct ddog_prof_ProfileExporter exporter = exporter_result.ok;

      struct ddog_prof_Request_Result req_result = ddog_prof_Exporter_Request_build(
          &exporter, &encoded, files_to_compress, files_unmodified, NULL, NULL, NULL);
      if (req_result.tag != DDOG_PROF_REQUEST_RESULT_OK_HANDLE_REQUEST) {
        ddog_CharSlice message = ddog_Error_message(&req_result.err);
        fprintf(stderr, "Failed to build request: %.*s\n", (int)message.len, message.ptr);
        ddog_Error_drop(&req_result.err);
      } else {
        struct ddog_CancellationToken cancel = ddog_CancellationToken_new();
        struct ddog_prof_Result_HttpStatus send_result =
            ddog_prof_Exporter_send(&exporter, &req_result.ok, &cancel);
        ddog_CancellationToken_drop(&cancel);
        // todo: this is a horrible enum variant name, can we fix this?
        if (send_result.tag != DDOG_PROF_RESULT_HTTP_STATUS_OK_HTTP_STATUS) {
          ddog_CharSlice message = ddog_Error_message(&send_result.err);
          fprintf(stderr, "Failed to send request: %.*s\n", (int)message.len, message.ptr);
          ddog_Error_drop(&send_result.err);
        } else {
          printf("[profiles.c] export complete (HTTP %u)\n", send_result.ok.code);
        }
      }
      ddog_prof_Exporter_drop(&exporter);

      // Reset interval: drop and recreate scratchpad and adapter.
      // The profiles dictionary lives forever in this example.
      ddog_prof_ProfileAdapter_drop(&adapter);
      ddog_prof_ScratchPad_drop(&scratch);
      check_ok(ddog_prof_ScratchPad_new(&scratch), "ScratchPad_new(restart)");
      rebuild_scratchpad_state(scratch, func_id, map_id, &stack_id);
      check_ok(ddog_prof_ProfileAdapter_new(
                   &adapter, dict, scratch,
                   (struct ddog_prof_Slice_ValueType){.ptr = value_types, .len = 3},
                   (struct ddog_Slice_I64){.ptr = groupings, .len = 3}),
               "ProfileAdapter_new(restart)");
      interval_started = now_secs;
      printf("[profiles.c] reset interval state for next interval\n");
    }

    // Sleep ~10ms, obviously this will drift, this is just an example.
    sleep_ms((unsigned)(WALL_TICK_NS / 1000000));
  }

  printf("[profiles.c] shutting down\n");
  ddog_prof_ProfileAdapter_drop(&adapter);
  ddog_Vec_Tag_drop(tags);
  ddog_prof_ScratchPad_drop(&scratch);
  ddog_prof_ProfilesDictionary_drop(&dict);
  return 0;
}
