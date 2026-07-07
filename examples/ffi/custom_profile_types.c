// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// Example: creating a profile with custom (type, unit) pairs.
//
// Use ddog_prof_Profile_new_custom when the desired sample type is not yet in
// the ddog_prof_SampleType enum.  All type/unit strings must be program-lifetime
// constants (string literals in practice).
//
// TODO: Once custom profile types are stable and agreed upon across profiler
// teams, add them to ddog_prof_SampleType and switch callers to
// ddog_prof_Profile_new / ddog_prof_Profile_with_dictionary.

#include <datadog/profiling.h>
#include <stdio.h>
#include <stdlib.h>

static int check_profile_result(ddog_prof_Profile_Result result, const char *context) {
  if (result.tag != DDOG_PROF_PROFILE_RESULT_OK) {
    ddog_CharSlice msg = ddog_Error_message(&result.err);
    fprintf(stderr, "%s: %.*s\n", context, (int)msg.len, msg.ptr);
    ddog_Error_drop(&result.err);
    return 0;
  }
  return 1;
}

int main(void) {
  // -------------------------------------------------------------------------
  // 1. Declare custom sample types as static string literals.
  //    These bypass the ddog_prof_SampleType enum so that new types can be
  //    prototyped without a libdatadog release.
  // -------------------------------------------------------------------------
  const ddog_prof_CustomValueType sample_types[] = {
      {.type_str = DDOG_CHARSLICE_C("memory-breakdown"),
       .unit = DDOG_CHARSLICE_C("bytes")},
  };
  const ddog_prof_Slice_CustomValueType sample_types_slice = {
      .ptr = sample_types,
      .len = sizeof(sample_types) / sizeof(sample_types[0]),
  };

  // The profile period is optional.  Pass NULL when there is no meaningful
  // sampling interval to report for the custom type.

  // -------------------------------------------------------------------------
  // 2. Create the profile.
  // -------------------------------------------------------------------------
  ddog_prof_Profile_NewResult new_result =
      ddog_prof_Profile_new_custom(sample_types_slice, NULL);
  if (new_result.tag != DDOG_PROF_PROFILE_NEW_RESULT_OK) {
    ddog_CharSlice msg = ddog_Error_message(&new_result.err);
    fprintf(stderr, "ddog_prof_Profile_new_custom failed: %.*s\n", (int)msg.len, msg.ptr);
    ddog_Error_drop(&new_result.err);
    return EXIT_FAILURE;
  }
  ddog_prof_Profile profile = new_result.ok;

  if (!check_profile_result(
          ddog_prof_Profile_set_omit_local_root_span_id_when_serializing(&profile, true),
          "set_omit_local_root_span_id")) {
    ddog_prof_Profile_drop(&profile);
    return EXIT_FAILURE;
  }

  // -------------------------------------------------------------------------
  // 3. Add a sample (one value per sample type).
  //    Separate stacks / labels can distinguish anonymous, file-backed, JIT,
  //    or other memory categories while sharing the same sample type.
  // -------------------------------------------------------------------------
  ddog_prof_Location location = {
      .mapping = (ddog_prof_Mapping){0},
      .function = {.name = DDOG_CHARSLICE_C("my_alloc_function"),
                   .filename = DDOG_CHARSLICE_C("/src/allocator.c")},
  };
  int64_t values[] = {4096};
  const ddog_prof_Sample sample = {
      .locations = {&location, 1},
      .values = {values, sizeof(values) / sizeof(values[0])},
      .labels = {NULL, 0},
  };

  if (!check_profile_result(ddog_prof_Profile_add(&profile, sample, 0),
                            "ddog_prof_Profile_add")) {
    ddog_prof_Profile_drop(&profile);
    return EXIT_FAILURE;
  }

  // -------------------------------------------------------------------------
  // 4. Serialize and verify we get back a non-empty buffer.
  // -------------------------------------------------------------------------
  ddog_prof_Profile_SerializeResult ser_result =
      ddog_prof_Profile_serialize(&profile, NULL, NULL);
  if (ser_result.tag != DDOG_PROF_PROFILE_SERIALIZE_RESULT_OK) {
    ddog_CharSlice msg = ddog_Error_message(&ser_result.err);
    fprintf(stderr, "serialize failed: %.*s\n", (int)msg.len, msg.ptr);
    ddog_Error_drop(&ser_result.err);
    ddog_prof_Profile_drop(&profile);
    return EXIT_FAILURE;
  }
  ddog_prof_EncodedProfile encoded = ser_result.ok;

  ddog_prof_Result_ByteSlice buf_result = ddog_prof_EncodedProfile_bytes(&encoded);
  if (buf_result.tag != DDOG_PROF_RESULT_BYTE_SLICE_OK_BYTE_SLICE) {
    ddog_CharSlice msg = ddog_Error_message(&buf_result.err);
    fprintf(stderr, "EncodedProfile_bytes failed: %.*s\n", (int)msg.len, msg.ptr);
    ddog_Error_drop(&buf_result.err);
    ddog_prof_EncodedProfile_drop(&encoded);
    ddog_prof_Profile_drop(&profile);
    return EXIT_FAILURE;
  }

  if (buf_result.ok.len == 0) {
    fprintf(stderr, "serialize returned an empty buffer\n");
    ddog_prof_EncodedProfile_drop(&encoded);
    ddog_prof_Profile_drop(&profile);
    return EXIT_FAILURE;
  }

  fprintf(stdout, "custom_profile_types: serialized %zu bytes\n", (size_t)buf_result.ok.len);

  // -------------------------------------------------------------------------
  // 5. Clean up.
  // -------------------------------------------------------------------------
  ddog_prof_EncodedProfile_drop(&encoded);
  ddog_prof_Profile_drop(&profile);
  return EXIT_SUCCESS;
}
