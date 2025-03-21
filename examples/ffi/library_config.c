// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/common.h>
#include <datadog/library-config.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
// Define setenv for Windows
#include <windows.h>
int setenv(const char *name, const char *value, int overwrite) {
  if (overwrite || getenv(name) == NULL) {
    return SetEnvironmentVariable(name, value) ? 0 : -1;
  }
  return 0;
}
#endif

#define DDOG_VAL_STR_PTR(val)                                                                      \
  (val.tag == DDOG_LIBRARY_CONFIG_VALUE_STR_VAL ? val.str_val.ptr : "\0")

#define DDOG_SLICE_CHARSLICE(arr)                                                                  \
  ((ddog_Slice_CharSlice){.ptr = arr, .len = sizeof(arr) / sizeof(arr[0])})

ddog_CStr from_null_terminated(char *str) { return (ddog_CStr){.ptr = str, .length = strlen(str)}; }

struct arguments {
  bool infer;
  bool help;
  ddog_CStr fleet_path;
  ddog_CStr local_path;
};

void parse_args(int argc, const char *const *argv, struct arguments *args) {
  args->infer = false;
  args->fleet_path = (ddog_CStr){0};
  args->local_path = (ddog_CStr){0};
  args->help = false;

  for (int i = 1; i < argc; i++) {
    if (strcmp(argv[i], "--infer") == 0) {
      args->infer = true;
    } else if (strcmp(argv[i], "--fleet-path") == 0) {
      if (i + 1 < argc) {
        args->fleet_path = from_null_terminated((char *)argv[i + 1]);
        i++;
      }
    } else if (strcmp(argv[i], "--local-path") == 0) {
      if (i + 1 < argc) {
        args->local_path = from_null_terminated((char *)argv[i + 1]);
        i++;
      }
    } else if (strcmp(argv[i], "--help") == 0) {
      args->help = true;
    }
  }
}

int main(int argc, const char *const *argv) {
  struct arguments args = {0};
  parse_args(argc, argv, &args);
  if (args.help) {
    printf("Usage: %s [--infer] [--fleet-path path] [--local-path path]\n",
           argc > 0 ? argv[0] : "library-config");
    return 0;
  }

  ddog_CharSlice language = DDOG_CHARSLICE_C("java");
  ddog_Configurator *configurator = ddog_library_configurator_new(true, language);

  if (args.infer) {
    ddog_library_configurator_with_detect_process_info(configurator);
  } else {
    ddog_CharSlice args[] = {
        DDOG_CHARSLICE_C("/bin/true"),
    };
    ddog_CharSlice envp[] = {
        DDOG_CHARSLICE_C("FOO=BAR"),
    };
    ddog_library_configurator_with_process_info(
        configurator, (ddog_ProcessInfo){.args = DDOG_SLICE_CHARSLICE(args),
                                         .envp = DDOG_SLICE_CHARSLICE(envp),
                                         .language = language});
  }

  if (args.local_path.ptr != NULL) {
    ddog_library_configurator_with_local_path(configurator, args.local_path);
  }
  if (args.fleet_path.ptr != NULL) {
    ddog_library_configurator_with_fleet_path(configurator, args.fleet_path);
  }

  ddog_Result_VecLibraryConfig config_result = ddog_library_configurator_get(configurator);

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

    printf("Setting env variable: %s=%s from origin %s\n", name.ptr, cfg->value.ptr,
           ddog_library_config_source_to_string(cfg->source).ptr);
    setenv(name.ptr, cfg->value.ptr, 1);
  }
}
