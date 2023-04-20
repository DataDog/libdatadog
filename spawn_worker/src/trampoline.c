// Unless explicitly stated otherwise all files in this repository are licensed under the Apache
// License Version 2.0. This product includes software developed at Datadog
// (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifndef _WIN32
#include <dlfcn.h>
#endif

int main(int argc, char *argv[]) {
  if (argc > 2) {
    const char *library_path = argv[1];

    // Last entry is always the symbol name
    const char *symbol_name = argv[argc-1];

    if (strcmp("__dummy_mirror_test", library_path) == 0) {
      printf("%s %s", library_path, symbol_name);
      return 0;
    }
#ifndef _WIN32
    int additional_shared_libraries_cnt = argc - 3;
    void **handles = NULL;

    if (additional_shared_libraries_cnt > 0) {
      handles = calloc(additional_shared_libraries_cnt, sizeof(void *));
    }

    for (int i = 0; i < additional_shared_libraries_cnt; i++) {
      handles[i] = dlopen(argv[2 + i], RTLD_LAZY | RTLD_GLOBAL);
      if (!handles[i]) {
        fputs(dlerror(), stderr);
        return 9;
      }
    }

    void *handle = dlopen(library_path, RTLD_LAZY);
    if (!handle) {
      fputs(dlerror(), stderr);
      return 10;
    }

    void (*fn)() = dlsym(handle, symbol_name);
    char *error = NULL;

    if ((error = dlerror()) != NULL) {
      fputs(error, stderr);
      return 11;
    }
    (*fn)();
    dlclose(handle);

    if (handles != NULL) {
      for (int i = 0; i < additional_shared_libraries_cnt; i++) {
        dlclose(handles[i]);
      }
      free(handles);
    }
#endif
    return 0;
  }

  return 9;
}