// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdbool.h>
#ifndef _WIN32
#include <dlfcn.h>
#include <fcntl.h>
#include <unistd.h>
#else
#include <windows.h>
#define unlink _unlink
#endif

static inline FILE *error_fd() {
    char *log_env = getenv("DD_TRACE_LOG_FILE");
    if (log_env) {
        FILE *file = fopen(log_env, "a");
        if (file) {
            return file;
        }
    }
    return stderr;
}

struct trampoline_data {
  int argc;
  char **argv;
  char **dependency_paths;
};

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

    int additional_shared_libraries_args = argc - 4;

    struct trampoline_data startup_data;
    startup_data.argc = argc;
    startup_data.argv = argv;
    startup_data.dependency_paths = NULL;

    int i;
#ifndef _WIN32
    void **handles = NULL;
#ifdef __GLIBC__
    void *librt_handle = NULL;
#endif
    if (additional_shared_libraries_args > 0) {
      handles = calloc(additional_shared_libraries_args, sizeof(void *));
      startup_data.dependency_paths = calloc(additional_shared_libraries_args + 1, sizeof(char *));

#ifdef __GLIBC__
      // appsec needs librt for shm_open, but doesn't declare needing it for compat with musl.
      // RTDL_LAZY has no effect because of the elf flag BIND_NOW
      librt_handle = dlopen("librt.so.1", RTLD_LAZY | RTLD_GLOBAL);
#endif
    }

    int additional_shared_libraries_count = 0;
    bool unlink_next = false;
    for (i = 0; i < additional_shared_libraries_args; i++) {
      const char *lib_path = argv[3 + i];
      if (*lib_path == '-' && !lib_path[1]) {
        unlink_next = true;
        continue;
      }
      bool already_loaded = false;
#ifndef _WIN32
      char buf[30];
      // Redirect the symlinked /dev/fd/X to the actual /proc/<pid>/X - as otherwise debugging tooling may try to read it
      // And reading /proc/self from the debugging tooling will usually lead to it reading from itself, which may be flatly wrong
      // E.g. gdb will just hang up for e.g. /proc/self/fd/4, which is an open pipe...
      if (strncmp(lib_path, "/dev/fd/", strlen("/dev/fd/")) == 0 && strlen(lib_path) < 20) {
        sprintf(buf, "/proc/%d/fd/%s", getpid(), lib_path + strlen("/dev/fd/"));
        if ((handles[additional_shared_libraries_count] = dlopen(buf, RTLD_LAZY | RTLD_GLOBAL))) {
          already_loaded = true;
        } else {
          // We may have to retry this (via already_loaded = false) in environments where procfs updates have some delay
          // Like observed on google cloud run platforms: /dev/fd/ exists, but /proc/<pid>/ does not yet.
          // clear any previous errors
          (void)dlerror();
        }
      } else if (unlink_next) {
        // try to keep the shared library alive as a file descriptor for later use
        int fd = open(lib_path, O_RDONLY);
        sprintf(buf, "/proc/%d/fd/%d", getpid(), fd);
        if ((handles[additional_shared_libraries_count] = dlopen(buf, RTLD_LAZY | RTLD_GLOBAL))) {
          already_loaded = true;
        } else {
          // clear any previous errors, and clean this up
          (void)dlerror();
          close(fd);
        }
      }
      if (already_loaded) {
        startup_data.dependency_paths[additional_shared_libraries_count] = strdup(buf);
      }
#endif
      if (!already_loaded) {
        if (!(handles[additional_shared_libraries_count] = dlopen(lib_path, RTLD_LAZY | RTLD_GLOBAL))) {
          fputs(dlerror(), error_fd());
          return 9;
        }
        if (!unlink_next) {
          startup_data.dependency_paths[additional_shared_libraries_count] = strdup(lib_path);
        } else {
          startup_data.dependency_paths[additional_shared_libraries_count] = NULL;
        }
      }
      if (unlink_next) {
        unlink(lib_path);
        unlink_next = false;
      }
      ++additional_shared_libraries_count;
    }
    if (startup_data.dependency_paths) {
      startup_data.dependency_paths[additional_shared_libraries_count] = NULL;
    }

    void *handle = dlopen(library_path, RTLD_LAZY | RTLD_GLOBAL);
    if (!handle) {
      fputs(dlerror(), error_fd());
      return 10;
    }

    // clear any previous errors
    (void)dlerror();

    void (*fn)(struct trampoline_data *) = dlsym(handle, symbol_name);
    char *error = NULL;

    if ((error = dlerror()) != NULL) {
      fputs(error, error_fd());
      return 11;
    }

    if (fn == NULL) {
      fprintf(error_fd(), "fn was not found; missing %s in %s", symbol_name, library_path);
      return 12;
    }

    (*fn)(&startup_data);
    dlclose(handle);

    if (handles != NULL) {
      for (i = 0; i < additional_shared_libraries_count; i++) {
        dlclose(handles[i]);
        if (startup_data.dependency_paths[i]) {
          free(startup_data.dependency_paths[i]);
        }
      }
      free(handles);
    }
#ifdef __GLIBC__
    if (librt_handle) {
      dlclose(librt_handle);
    }
#endif
#else
    for (i = 0; i < additional_shared_libraries_args; i++) {
        const char *lib_path = argv[3 + i];
        HINSTANCE handle = LoadLibrary(lib_path);
        if (!handle) {
            DWORD res = GetLastError();
            fprintf(error_fd(), "error: %lu, could not load dependent shared library %s\n", res, lib_path);
            return 9;
        }
    }

    HINSTANCE handle = LoadLibrary(library_path);
    if (!handle) {
        DWORD res = GetLastError();
        fprintf(error_fd(), "error: %lu, could not load shared library %s\n", res, library_path);
        return 10;
    } 

    void (*fn)(struct trampoline_data *) = (void(*)(struct trampoline_data *))GetProcAddress(handle, symbol_name);

    if (!fn) {
        DWORD res = GetLastError();
        fprintf(error_fd(), "error: %lu loading symbol: %s from: %s\n", res, symbol_name, library_path);
        return 11;
    }

    (*fn)(&startup_data);
#endif
    return 0;
  }

  return 13;
}
