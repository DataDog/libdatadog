// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/common.h>
#include <datadog/library-config.h>
#include <stdio.h>
#include <stdlib.h>

#define DDOG_VAL_STR_PTR(val)                                                                      \
  (val.tag == DDOG_LIBRARY_CONFIG_VALUE_STR_VAL ? val.str_val.ptr : "\0")

#define DDOG_SLICE_CHARSLICE(arr)                                                                  \
  ((ddog_Slice_CharSlice){.ptr = arr, .len = sizeof(arr) / sizeof(arr[0])})

int main(int argc, const char *const *argv) {
  ddog_Configurator *configurator = ddog_library_configurator_new(true);
  ddog_library_configurator_with_path(configurator,
                                      DDOG_CHARSLICE_C("/tmp/foobar/static_config.yaml"));

  ddog_CharSlice args[] = {
      DDOG_CHARSLICE_C("/bin/true"),
  };
  ddog_CharSlice envp[] = {
      DDOG_CHARSLICE_C("FOO=BAR"),
  };
  ddog_Result_VecLibraryConfig config_result = ddog_library_configurator_get(
      configurator, (ddog_ProcessInfo){.args = DDOG_SLICE_CHARSLICE(args),
                                       .envp = DDOG_SLICE_CHARSLICE(envp),
                                       .language = DDOG_CHARSLICE_C("java")});
  if (config_result.tag == DDOG_RESULT_VEC_LIBRARY_CONFIG_ERR_VEC_LIBRARY_CONFIG) {
    ddog_Error err = config_result.err;
    fprintf(stderr, "%.*s", (int)err.message.len, err.message.ptr);
    ddog_Error_drop(&err);
    exit(1);
  }

  ddog_Vec_LibraryConfig configs = config_result.ok;
  for (int i = 0; i < configs.len; i++) {
    const ddog_LibraryConfig *cfg = &configs.ptr[i];
    ddog_CStr name = ddog_library_config_name_to_env(cfg->name);

    printf("Setting env variable: %s=%s\n", name.ptr, DDOG_VAL_STR_PTR(cfg->value));
    setenv(name.ptr, DDOG_VAL_STR_PTR(cfg->value), 1);
  }
}
