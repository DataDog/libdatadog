// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present
// Datadog, Inc.

#include <ddprof/ffi.h>
#include <stdint.h>

/* Creates a profile with one sample type "wall-time" with period of "wall-time"
 * with unit 60 "nanoseconds". Adds one sample with a string label "language".
 */
int main(void) {
  const struct ddprof_ffi_ValueType wall_time = {
      .type_ = DDPROF_FFI_CHARSLICE_C("wall-time"),
      .unit = DDPROF_FFI_CHARSLICE_C("nanoseconds"),
  };
  const struct ddprof_ffi_Slice_value_type sample_types = {&wall_time, 1};
  const struct ddprof_ffi_Period period = {wall_time, 60};
  ddprof_ffi_Profile *profile = ddprof_ffi_Profile_new(sample_types, &period);

  struct ddprof_ffi_Line root_line = {
      .function =
          (struct ddprof_ffi_Function){
              .name = DDPROF_FFI_CHARSLICE_C("{main}"),
              .filename = DDPROF_FFI_CHARSLICE_C("/srv/example/index.php"),
          },
      .line = 0,
  };

  struct ddprof_ffi_Location root_location = {
      // yes, a zero-initialized mapping is valid
      .mapping = (struct ddprof_ffi_Mapping){},
      .lines = (struct ddprof_ffi_Slice_line){&root_line, 1},
  };
  int64_t value = 10;
  const struct ddprof_ffi_Label label = {
      .key = DDPROF_FFI_CHARSLICE_C("language"),
      .str = DDPROF_FFI_CHARSLICE_C("php"),
  };
  struct ddprof_ffi_Sample sample = {
      .locations = {&root_location, 1},
      .values = {&value, 1},
      .labels = {&label, 1},
  };
  ddprof_ffi_Profile_add(profile, sample);
  ddprof_ffi_Profile_free(profile);
  return 0;
}
