// Unless explicitly stated otherwise all files in this repository are licensed under the Apache
// License Version 2.0. This product includes software developed at Datadog
// (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdbool.h>
#ifndef _WIN32
#include <dlfcn.h>
#include <unistd.h>
#else
#include <windows.h>
#define unlink _unlink
#endif

int main(int argc, char *argv[]) {
  if (argc > 3) {
    // remove the temp file of this trampoline
    if (*argv[1]) {
      unlink(argv[1]);
    }

    const char *library_path = argv[2];

    // Last entry is always the symbol name
    const char *symbol_name = argv[argc - 1];

    if (strcmp("__dummy_mirror_test", library_path) == 0) {
      printf("%s %s", library_path, symbol_name);
      return 0;
    }
#ifndef _WIN32
    int additional_shared_libraries_args = argc - 4;
    void **handles = NULL;

    if (additional_shared_libraries_args > 0) {
      handles = calloc(additional_shared_libraries_args, sizeof(void *));
    }

    int additional_shared_libraries_count = 0;
    bool unlink_next = false;
    for (int i = 0; i < additional_shared_libraries_args; i++) {
      const char *lib_path = argv[3 + i];
      if (*lib_path == '-' && !lib_path[1]) {
          unlink_next = true;
          continue;
      }
      if (!(handles[additional_shared_libraries_count++] = dlopen(lib_path, RTLD_LAZY | RTLD_GLOBAL))) {
          fputs(dlerror(), stderr);
          return 9;
      }
      if (unlink_next) {
        unlink(lib_path);
        unlink_next = false;
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
      for (int i = 0; i < additional_shared_libraries_count; i++) {
        dlclose(handles[i]);
      }
      free(handles);
    }
#else
    HINSTANCE handle = LoadLibrary(library_path);
    if (!handle) {
        DWORD res = GetLastError();
        fprintf(stderr, "error: %i, could not load shared library\n", res);
        return 10;
    } 

    void (*fn)() = GetProcAddress(handle, symbol_name);

    if (!fn) {
        DWORD res = GetLastError();
        fprintf(stderr, "error: %i loading symbol: %s from: %s\n", res, symbol_name, library_path);
        return 11;
    }

    (*fn)();
#endif
    return 0;
  }

  return 9;
}