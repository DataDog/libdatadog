// Unless explicitly stated otherwise all files in this repository are licensed under the Apache
// License Version 2.0. This product includes software developed at Datadog
// (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
#ifndef _WIN32
#define _GNU_SOURCE

#define UNUSED(x) (void)(x)
#include <dlfcn.h>
#include <stdio.h>
#include <string.h>

int main_override(int argc, char **argv) {
  if (argc > 2) {
    // const char *_library_path = argv[1];
    const char *symbol_name = argv[2];
    void (*fn)() = dlsym(RTLD_DEFAULT, symbol_name);
    char *error = NULL;

    if ((error = dlerror()) != NULL) {
      fputs(error, stderr);
      return 31;
    }

    (*fn)();
  }
  return 0;
}

// meant to be used for overriding using LD_PRELOAD
//
// allows executables to be hijacked to execute alternative entry points
int __libc_start_main(int (*main)(int, char **), int argc, char **argv,
                      int (*init)(int, char **, char **), void (*fini)(void),
                      void (*rtld_fini)(void), void *stack_end) {
  UNUSED(main);
  typeof(&__libc_start_main) libc_start_main = dlsym(RTLD_NEXT, "__libc_start_main");

  return libc_start_main(main_override, argc, argv, init, fini, rtld_fini, stack_end);
}

#endif